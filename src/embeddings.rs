//! Local feature-hash embeddings + RRF hybrid search (v0.4).
//! No network, no model download, no new dependency: symbol text is embedded
//! with the classic "hashing trick" (signed feature hashing), entirely offline
//! and reproducible byte-for-byte across machines and releases.
//!
//! Default `search` stays pure FTS (query.rs). `hybrid_search` here is additive:
//! it fuses the FTS ranked list with an embedding-similarity ranked list via
//! Reciprocal Rank Fusion, and only does anything useful once `materialize` has
//! populated the frozen `embeddings` table. Empty table → FTS-only, silently.

use anyhow::Result;
use rusqlite::Connection;

/// Embedding dimensionality. Small on purpose: symbol names/signatures are
/// short documents, and a tiny fixed-size vector keeps this "local" honest
/// (no ANN index needed — brute-force cosine over a few thousand nodes is fine).
const DIM: usize = 256;

/// RRF constant. 60 is the standard value from the original RRF paper — it
/// flattens the influence of exact rank position so neither list dominates.
const RRF_K: f64 = 60.0;

/// How many embedding-similarity hits to pull before RRF fusion.
const EMBED_TOPN: usize = 50;

/// FNV-1a 64-bit, fixed offset/prime. Hand-written (not `DefaultHasher`, which
/// is explicitly *not* stable across Rust releases) so embeddings computed
/// today reproduce identically on any machine, any version, forever.
const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

fn fnv1a(s: &str) -> u64 {
    let mut h = FNV_OFFSET;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Split into lowercase alphanumeric tokens, further splitting camelCase and
/// snake_case/kebab-case boundaries so `getUserById` and `user_id` both yield
/// `["get","user","by","id"]` / `["user","id"]`. Pure ASCII-aware; anything
/// non-alphanumeric is a separator.
fn tokens(doc: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;

    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if !cur.is_empty() {
            out.push(std::mem::take(cur));
        }
    };

    for c in doc.chars() {
        if c.is_alphanumeric() {
            // camelCase boundary: lower/digit followed by upper starts a new token.
            if c.is_uppercase() && prev_lower {
                flush(&mut cur, &mut out);
            }
            cur.push(c.to_ascii_lowercase());
            prev_lower = c.is_lowercase() || c.is_numeric();
        } else {
            flush(&mut cur, &mut out);
            prev_lower = false;
        }
    }
    flush(&mut cur, &mut out);
    out.retain(|t| !t.is_empty());
    out
}

/// Char 3-grams of a (lowercased) name, for fuzzy/typo robustness — these are
/// blended into the same bag of tokens as the word-level splits so a query
/// like "usr" still has partial overlap with "user".
fn char_trigrams(name: &str) -> Vec<String> {
    let lower = name.to_ascii_lowercase();
    let chars: Vec<char> = lower.chars().filter(|c| c.is_alphanumeric()).collect();
    if chars.len() < 3 {
        return Vec::new();
    }
    chars.windows(3).map(|w| w.iter().collect()).collect()
}

/// Build the full token bag for a "document" string (see `materialize` for how
/// the document is assembled per node) plus 3-grams of the leading word (the
/// symbol name is always the first field of the document).
fn tokenize_doc(doc: &str) -> Vec<String> {
    let mut toks = tokens(doc);
    if let Some(name) = doc.split(' ').next() {
        toks.extend(char_trigrams(name));
    }
    toks
}

/// Signed feature hashing: each token votes +1/-1 into `bucket = hash % DIM`,
/// sign from a second (independent) hash bit. Accumulating this way lets
/// unrelated collisions cancel out on average instead of always adding up.
/// Result is L2-normalized so stored vectors are directly dot-product-comparable.
fn embed(doc: &str) -> Vec<f32> {
    let mut v = vec![0f32; DIM];
    for tok in tokenize_doc(doc) {
        let h = fnv1a(&tok);
        let bucket = (h % DIM as u64) as usize;
        // Second, independent hash (salted) picks the sign bit.
        let sign_bit = fnv1a(&format!("{tok}#sign")) & 1;
        let sign = if sign_bit == 0 { 1.0 } else { -1.0 };
        v[bucket] += sign;
    }
    l2_normalize(&mut v);
    v
}

fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    // Both sides are already L2-normalized (stored + query), so plain dot
    // product *is* cosine similarity — no need to re-divide by norms.
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for x in v {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    bytes
}

fn blob_to_vec(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Build the embedding "document" for one symbol row exactly as specified:
/// name + qualified_name + signature + file_path (path carries package/folder
/// signal that pure name matching misses).
fn doc_for(name: &str, qualified_name: &str, signature: &str, file_path: &str) -> String {
    format!("{name} {qualified_name} {signature} {file_path}")
}

/// Compute + store a feature-hash embedding for every symbol node
/// (kind IN function/class/method — route/community/process nodes are skipped,
/// they don't carry the same kind of free text). Idempotent: clears the table
/// and re-inserts, so this is safe to call after every index write.
pub fn materialize(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM embeddings", [])?;
    {
        let mut sel = tx.prepare(
            "SELECT id, name, qualified_name, file_path, signature FROM nodes
             WHERE kind IN ('function','class','method')",
        )?;
        let mut ins =
            tx.prepare("INSERT OR REPLACE INTO embeddings(node_id, dim, vec) VALUES(?1,?2,?3)")?;
        let rows = sel.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (id, name, qualified_name, file_path, signature) = row?;
            let doc = doc_for(&name, &qualified_name, &signature, &file_path);
            let v = embed(&doc);
            ins.execute((&id, DIM as i64, vec_to_blob(&v)))?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// FTS side of the fusion: sanitize + MATCH `nodes_fts`, falling back to a
/// LIKE scan on `nodes.name` if MATCH errors (FTS5 query syntax is picky about
/// operators like `-`, `"`, `*` appearing in raw user input). Never panics.
fn fts_ranked(conn: &Connection, query: &str, limit: usize) -> Vec<String> {
    let sanitized = sanitize_fts_query(query);
    if !sanitized.is_empty() {
        let sql = "SELECT node_id FROM nodes_fts WHERE nodes_fts MATCH ?1 LIMIT ?2";
        if let Ok(mut stmt) = conn.prepare(sql) {
            if let Ok(rows) = stmt.query_map(rusqlite::params![sanitized, limit as i64], |r| {
                r.get::<_, String>(0)
            }) {
                let ids: Vec<String> = rows.filter_map(|r| r.ok()).collect();
                if !ids.is_empty() {
                    return ids;
                }
            }
        }
    }
    // Fallback: LIKE scan on name. Also catches MATCH returning zero rows on
    // a query FTS tokenized away to nothing (e.g. pure punctuation).
    let like = format!("%{}%", query.replace('%', "").replace('_', ""));
    let sql = "SELECT id FROM nodes WHERE name LIKE ?1 LIMIT ?2";
    conn.prepare(sql)
        .and_then(|mut stmt| {
            stmt.query_map(rusqlite::params![like, limit as i64], |r| {
                r.get::<_, String>(0)
            })
            .map(|it| it.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
}

/// Strip FTS5 operator syntax that would otherwise throw a MATCH parse error
/// on arbitrary user input (quotes, `-`/`+` prefixes, `*`, `:`, parens, `^`).
/// What's left is just whitespace-separated bareword tokens, which FTS5
/// always accepts as an implicit AND query.
fn sanitize_fts_query(q: &str) -> String {
    q.chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Embedding side of the fusion: embed the query with the same feature-hash
/// function used at index time, load all stored vectors, rank by cosine.
fn embedding_ranked(conn: &Connection, query: &str, limit: usize) -> Vec<String> {
    let qv = embed(query);
    if qv.iter().all(|x| *x == 0.0) {
        return Vec::new(); // query tokenized to nothing (e.g. all punctuation)
    }
    let Ok(mut stmt) = conn.prepare("SELECT node_id, vec FROM embeddings") else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
    });
    let Ok(rows) = rows else { return Vec::new() };

    let mut scored: Vec<(String, f32)> = rows
        .filter_map(|r| r.ok())
        .map(|(id, blob)| {
            let v = blob_to_vec(&blob);
            (id, cosine(&qv, &v))
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(limit);
    scored.into_iter().map(|(id, _)| id).collect()
}

/// Reciprocal Rank Fusion: score(id) = sum over each ranked list of
/// 1/(k + rank), rank 1-based, k=60. An id absent from a list contributes 0
/// from that list. Ties broken by first-seen order (stable sort) — deterministic.
fn rrf_merge(lists: &[Vec<String>], limit: usize) -> Vec<String> {
    use std::collections::HashMap;
    let mut scores: HashMap<&str, f64> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for list in lists {
        for (i, id) in list.iter().enumerate() {
            let rank = (i + 1) as f64;
            let e = scores.entry(id.as_str()).or_insert_with(|| {
                order.push(id.as_str());
                0.0
            });
            *e += 1.0 / (RRF_K + rank);
        }
    }
    order.sort_by(|a, b| {
        scores[b]
            .partial_cmp(&scores[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    order.truncate(limit);
    order.into_iter().map(|s| s.to_string()).collect()
}

/// Hybrid search: fuse FTS ranking with embedding cosine similarity via RRF
/// (k=60). Returns node_ids best-first, capped at `limit`. If the embeddings
/// table is empty (v0.4 not yet materialized, or a repo with no symbols),
/// this degrades gracefully to FTS-only results — never an error, never a panic.
pub fn hybrid_search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<String>> {
    let fts_limit = limit.max(25);
    let fts = fts_ranked(conn, query, fts_limit);

    let has_embeddings: i64 = conn
        .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
        .unwrap_or(0);
    if has_embeddings == 0 {
        let mut out = fts;
        out.truncate(limit);
        return Ok(out);
    }

    let emb = embedding_ranked(conn, query, EMBED_TOPN);
    if fts.is_empty() && emb.is_empty() {
        return Ok(Vec::new());
    }
    Ok(rrf_merge(&[fts, emb], limit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_is_deterministic() {
        let a = embed("getUserById getUserById() src/users.ts");
        let b = embed("getUserById getUserById() src/users.ts");
        assert_eq!(a, b);
    }

    #[test]
    fn embed_differs_for_different_docs() {
        let a = embed("getUserById getUserById() src/users.ts");
        let b = embed("parseConfig parseConfig() src/config.ts");
        assert_ne!(a, b);
    }

    #[test]
    fn embed_is_l2_normalized() {
        let v = embed("deleteUserAccount deleteUserAccount() src/users.ts");
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norm was {norm}");
    }

    #[test]
    fn embed_of_empty_doc_is_zero_vector_not_nan() {
        // No tokens → nothing accumulated → normalize must not divide by zero.
        let v = embed("   ");
        assert!(v.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn blob_round_trip() {
        let v = embed("createOrder createOrder() src/orders.ts");
        let blob = vec_to_blob(&v);
        let back = blob_to_vec(&blob);
        assert_eq!(v, back);
    }

    #[test]
    fn tokenizer_splits_camel_and_snake_case() {
        assert_eq!(tokens("getUserById"), vec!["get", "user", "by", "id"]);
        assert_eq!(tokens("user_id"), vec!["user", "id"]);
        assert_eq!(tokens("parse-config"), vec!["parse", "config"]);
    }

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE nodes (
                id             TEXT PRIMARY KEY,
                kind           TEXT NOT NULL,
                name           TEXT NOT NULL,
                qualified_name TEXT NOT NULL,
                file_path      TEXT NOT NULL,
                start_line     INTEGER NOT NULL,
                end_line       INTEGER NOT NULL,
                exported       INTEGER NOT NULL DEFAULT 0,
                signature      TEXT NOT NULL DEFAULT ''
            );
            CREATE VIRTUAL TABLE nodes_fts USING fts5(
                name, qualified_name, file_path, node_id UNINDEXED
            );
            CREATE TABLE embeddings (
                node_id TEXT PRIMARY KEY,
                dim     INTEGER NOT NULL,
                vec     BLOB NOT NULL
            );
            "#,
        )
        .unwrap();
        conn
    }

    fn insert_symbol(conn: &Connection, id: &str, name: &str, sig: &str, file: &str) {
        conn.execute(
            "INSERT INTO nodes(id,kind,name,qualified_name,file_path,start_line,end_line,exported,signature)
             VALUES(?1,'function',?2,?2,?3,1,2,0,?4)",
            (id, name, file, sig),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nodes_fts(name, qualified_name, file_path, node_id) VALUES(?1,?1,?2,?3)",
            (name, file, id),
        )
        .unwrap();
    }

    #[test]
    fn hybrid_search_ranks_matching_symbols_above_unrelated_ones() {
        let mut conn = setup_db();
        insert_symbol(
            &conn,
            "id1",
            "getUserById",
            "getUserById(id)",
            "src/users.ts",
        );
        insert_symbol(&conn, "id2", "deleteUser", "deleteUser(id)", "src/users.ts");
        insert_symbol(
            &conn,
            "id3",
            "parseConfig",
            "parseConfig(path)",
            "src/config.ts",
        );

        materialize(&mut conn).unwrap();

        let results = hybrid_search(&conn, "user", 10).unwrap();
        assert!(!results.is_empty());

        let pos = |id: &str| results.iter().position(|r| r == id);
        let p1 = pos("id1").expect("getUserById should match");
        let p2 = pos("id2").expect("deleteUser should match");
        let p3 = pos("id3");

        // Both user-related symbols should outrank (or entirely exclude) parseConfig.
        match p3 {
            Some(p3) => {
                assert!(p1 < p3, "getUserById should rank above parseConfig");
                assert!(p2 < p3, "deleteUser should rank above parseConfig");
            }
            None => {} // parseConfig didn't match at all — also acceptable.
        }
    }

    #[test]
    fn hybrid_search_falls_back_to_fts_only_when_no_embeddings() {
        let conn = setup_db();
        insert_symbol(
            &conn,
            "id1",
            "getUserById",
            "getUserById(id)",
            "src/users.ts",
        );
        // materialize() not called — embeddings table stays empty.
        let results = hybrid_search(&conn, "user", 10).unwrap();
        assert_eq!(results, vec!["id1".to_string()]);
    }

    #[test]
    fn hybrid_search_never_panics_on_weird_query_input() {
        let mut conn = setup_db();
        insert_symbol(
            &conn,
            "id1",
            "getUserById",
            "getUserById(id)",
            "src/users.ts",
        );
        materialize(&mut conn).unwrap();

        for q in [
            "",
            "   ",
            "\"unterminated",
            "-*:()^",
            "AND OR NOT",
            "a".repeat(500).as_str(),
        ] {
            let _ = hybrid_search(&conn, q, 10);
        }
    }
}
