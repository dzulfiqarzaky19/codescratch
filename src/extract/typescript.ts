import type { Node as SyntaxNode } from "web-tree-sitter";
import type {
  ExtractedBinding,
  ExtractedRef,
  ExtractedSymbol,
  FileExtraction,
} from "../models.js";
import { parseSource } from "./parser.js";

type Node = SyntaxNode;

export async function extractTypeScriptFile(
  filePath: string,
  source: string,
  language: string,
): Promise<FileExtraction> {
  const tree = await parseSource(source, language);
  try {
    const symbols: ExtractedSymbol[] = [];
    const refs: ExtractedRef[] = [];
    const bindings: ExtractedBinding[] = [];
    const exportedNames = collectExportNames(tree.rootNode);

    walk(tree.rootNode, null, symbols, refs, bindings, exportedNames, source);
    return { filePath, language, symbols, refs, bindings };
  } finally {
    tree.delete();
  }
}

function walk(
  node: Node,
  enclosing: string | null,
  symbols: ExtractedSymbol[],
  refs: ExtractedRef[],
  bindings: ExtractedBinding[],
  exportedNames: Set<string>,
  source: string,
): void {
  switch (node.type) {
    case "function_declaration":
    case "generator_function_declaration": {
      const name = childText(node, "name");
      if (name) {
        const qn = qualify(enclosing, name);
        symbols.push({
          kind: "function",
          name,
          qualifiedName: qn,
          startLine: node.startPosition.row + 1,
          endLine: node.endPosition.row + 1,
          exported: exportedNames.has(name) || hasExportModifier(node),
          signature: firstLine(source, node),
        });
        walkChildren(node, qn, symbols, refs, bindings, exportedNames, source);
        return;
      }
      break;
    }
    case "class_declaration":
    case "abstract_class_declaration": {
      const name = childText(node, "name");
      if (name) {
        const qn = qualify(enclosing, name);
        symbols.push({
          kind: "class",
          name,
          qualifiedName: qn,
          startLine: node.startPosition.row + 1,
          endLine: node.endPosition.row + 1,
          exported: exportedNames.has(name) || hasExportModifier(node),
          signature: firstLine(source, node),
        });
        for (const child of namedChildren(node)) {
          if (
            child.type === "class_heritage" ||
            child.type === "extends_clause" ||
            child.type === "implements_clause"
          ) {
            extractHeritage(child, qn, refs);
          }
        }
        walkChildren(node, qn, symbols, refs, bindings, exportedNames, source);
        return;
      }
      break;
    }
    case "method_definition":
    case "method_signature": {
      const name = methodName(node);
      if (name && enclosing && methodOwnerOk(node)) {
        const qn = `${enclosing}.${name}`;
        symbols.push({
          kind: "method",
          name,
          qualifiedName: qn,
          startLine: node.startPosition.row + 1,
          endLine: node.endPosition.row + 1,
          exported: false,
          signature: firstLine(source, node),
          parentQualifiedName: enclosing,
        });
        refs.push({
          kind: "has_method",
          srcQualifiedName: enclosing,
          rawName: name,
          targetHint: qn,
          line: node.startPosition.row + 1,
        });
        walkChildren(node, qn, symbols, refs, bindings, exportedNames, source);
        return;
      }
      break;
    }
    case "public_field_definition":
    case "field_definition": {
      // class Foo { bar = () => {} } or bar() via property
      if (!enclosing) break;
      const name = methodName(node) ?? childText(node, "name");
      if (!name) break;
      const value = node.childForFieldName("value");
      const isFn =
        !!value &&
        (value.type === "arrow_function" ||
          value.type === "function_expression" ||
          value.type === "generator_function");
      if (!isFn) break;
      const qn = `${enclosing}.${name}`;
      symbols.push({
        kind: "method",
        name,
        qualifiedName: qn,
        startLine: node.startPosition.row + 1,
        endLine: node.endPosition.row + 1,
        exported: false,
        signature: firstLine(source, node),
        parentQualifiedName: enclosing,
      });
      refs.push({
        kind: "has_method",
        srcQualifiedName: enclosing,
        rawName: name,
        targetHint: qn,
        line: node.startPosition.row + 1,
      });
      if (value) {
        walkChildren(value, qn, symbols, refs, bindings, exportedNames, source);
      }
      return;
    }
    case "interface_declaration": {
      const name = childText(node, "name");
      if (name) {
        const qn = qualify(enclosing, name);
        symbols.push({
          kind: "interface",
          name,
          qualifiedName: qn,
          startLine: node.startPosition.row + 1,
          endLine: node.endPosition.row + 1,
          exported: exportedNames.has(name) || hasExportModifier(node),
          signature: firstLine(source, node),
        });
        for (const child of namedChildren(node)) {
          if (
            child.type === "extends_type_clause" ||
            child.type === "extends_clause"
          ) {
            extractHeritage(child, qn, refs, "extends");
          }
        }
        walkChildren(node, qn, symbols, refs, bindings, exportedNames, source);
        return;
      }
      break;
    }
    case "type_alias_declaration": {
      const name = childText(node, "name");
      if (name) {
        const qn = qualify(enclosing, name);
        symbols.push({
          kind: "type",
          name,
          qualifiedName: qn,
          startLine: node.startPosition.row + 1,
          endLine: node.endPosition.row + 1,
          exported: exportedNames.has(name) || hasExportModifier(node),
          signature: firstLine(source, node),
        });
      }
      break;
    }
    case "lexical_declaration":
    case "variable_declaration": {
      // enclosing is null inside an unnamed scope too (a callback arrow), so a
      // local there would be emitted as a top-level symbol and collide on
      // nodeId with every same-named local in the file. Require module level.
      if (!enclosing && atModuleLevel(node)) {
        for (const decl of namedChildren(node).filter(
          (c) => c.type === "variable_declarator",
        )) {
          const name = childText(decl, "name");
          if (!name) continue;
          const value = decl.childForFieldName("value");
          const isFn =
            !!value &&
            (value.type === "arrow_function" ||
              value.type === "function_expression" ||
              value.type === "generator_function");
          const qn = name;
          symbols.push({
            kind: isFn ? "function" : "variable",
            name,
            qualifiedName: qn,
            startLine: decl.startPosition.row + 1,
            endLine: decl.endPosition.row + 1,
            exported: exportedNames.has(name) || hasExportModifier(node),
            signature: firstLine(source, decl),
          });
          if (isFn && value) {
            walkChildren(
              value,
              qn,
              symbols,
              refs,
              bindings,
              exportedNames,
              source,
            );
          }
        }
        return;
      }
      break;
    }
    case "import_statement": {
      extractImport(node, refs, bindings);
      break;
    }
    case "export_statement": {
      extractExport(
        node,
        symbols,
        refs,
        bindings,
        exportedNames,
        source,
        enclosing,
      );
      walkChildren(
        node,
        enclosing,
        symbols,
        refs,
        bindings,
        exportedNames,
        source,
      );
      return;
    }
    case "call_expression": {
      const callee = node.childForFieldName("function");
      if (callee) {
        // dynamic import()
        if (callee.type === "import") {
          break;
        }
        const raw = calleeName(callee);
        if (raw && !isBuiltin(raw)) {
          refs.push({
            kind: "calls",
            srcQualifiedName: enclosing,
            rawName: raw,
            targetHint: raw,
            line: node.startPosition.row + 1,
          });
        }
      }
      break;
    }
    case "new_expression": {
      const ctor = node.childForFieldName("constructor");
      if (ctor) {
        const raw = calleeName(ctor);
        if (raw && !isBuiltin(raw)) {
          refs.push({
            kind: "calls",
            srcQualifiedName: enclosing,
            rawName: raw,
            targetHint: raw,
            line: node.startPosition.row + 1,
          });
        }
      }
      break;
    }
    default:
      break;
  }

  walkChildren(node, enclosing, symbols, refs, bindings, exportedNames, source);
}

function namedChildren(node: Node): Node[] {
  const out: Node[] = [];
  for (let i = 0; i < node.namedChildCount; i++) {
    const c = node.namedChild(i);
    if (c) out.push(c);
  }
  return out;
}

function walkChildren(
  node: Node,
  enclosing: string | null,
  symbols: ExtractedSymbol[],
  refs: ExtractedRef[],
  bindings: ExtractedBinding[],
  exportedNames: Set<string>,
  source: string,
): void {
  for (const child of namedChildren(node)) {
    walk(child, enclosing, symbols, refs, bindings, exportedNames, source);
  }
}

function extractImport(
  node: Node,
  refs: ExtractedRef[],
  bindings: ExtractedBinding[],
): void {
  // tree-sitter-typescript: source is often a bare `string` child, not always field "source"
  const sourceNode =
    node.childForFieldName("source") ??
    namedChildren(node).find((c) => c.type === "string");
  if (!sourceNode) return;
  const spec = stripQuotes(sourceNode.text);
  const line = node.startPosition.row + 1;
  const isTypeOnly = node.text.trimStart().startsWith("import type");

  refs.push({
    kind: "imports",
    srcQualifiedName: null,
    rawName: spec,
    targetHint: spec,
    line,
    isTypeOnly,
  });

  // Shape: import_statement → import_clause → (identifier | namespace_import | named_imports)
  const clause =
    namedChildren(node).find((c) => c.type === "import_clause") ?? node;

  for (const c of namedChildren(clause)) {
    if (c.type === "identifier") {
      bindings.push({
        localName: c.text,
        importedName: "default",
        moduleSpecifier: spec,
        isTypeOnly,
        isNamespace: false,
        line: c.startPosition.row + 1,
      });
    } else if (c.type === "namespace_import") {
      const ns =
        c.childForFieldName("name") ??
        namedChildren(c).find((x) => x.type === "identifier");
      if (ns) {
        bindings.push({
          localName: ns.text,
          importedName: "*",
          moduleSpecifier: spec,
          isTypeOnly,
          isNamespace: true,
          line: ns.startPosition.row + 1,
        });
      }
    } else if (c.type === "named_imports") {
      for (const specNode of c.descendantsOfType("import_specifier")) {
        if (!specNode) continue;
        // Fields may be absent; children are [imported] or [imported, local]
        const nameNode =
          specNode.childForFieldName("name") ??
          namedChildren(specNode).find((x) => x.type === "identifier");
        const aliasNode =
          specNode.childForFieldName("alias") ??
          namedChildren(specNode).filter((x) => x.type === "identifier")[1];
        if (!nameNode) continue;
        const importedName = nameNode.text;
        const localName = aliasNode ? aliasNode.text : importedName;
        const typeOnly =
          isTypeOnly || specNode.text.trimStart().startsWith("type ");
        bindings.push({
          localName,
          importedName,
          moduleSpecifier: spec,
          isTypeOnly: typeOnly,
          isNamespace: false,
          line: specNode.startPosition.row + 1,
        });
      }
    }
  }
}

function extractExport(
  node: Node,
  symbols: ExtractedSymbol[],
  refs: ExtractedRef[],
  bindings: ExtractedBinding[],
  exportedNames: Set<string>,
  source: string,
  enclosing: string | null,
): void {
  const sourceNode =
    node.childForFieldName("source") ??
    namedChildren(node).find((c) => c.type === "string");
  const fromSpec =
    sourceNode && sourceNode.type === "string"
      ? stripQuotes(sourceNode.text)
      : null;
  const line = node.startPosition.row + 1;

  if (fromSpec) {
    refs.push({
      kind: "imports",
      srcQualifiedName: null,
      rawName: fromSpec,
      targetHint: fromSpec,
      line,
    });
  }

  // export * from './mod'  |  export * as ns from './mod'
  const text = node.text.replace(/\s+/g, " ");
  const isStarReexport =
    !!fromSpec &&
    (/^export\s+\*\s+from\b/.test(text) ||
      /^export\s+type\s+\*\s+from\b/.test(text) ||
      namedChildren(node).some((c) => c.type === "namespace_export") ||
      (node.children.some((c) => c?.type === "*") &&
        !namedChildren(node).some((c) => c.type === "export_clause")));

  if (isStarReexport && fromSpec) {
    const nsExport = namedChildren(node).find(
      (c) => c.type === "namespace_export",
    );
    if (nsExport) {
      const ns =
        nsExport.childForFieldName("name") ??
        namedChildren(nsExport).find((x) => x.type === "identifier");
      if (ns) {
        bindings.push({
          localName: ns.text,
          importedName: "*",
          moduleSpecifier: fromSpec,
          isTypeOnly: text.includes("export type"),
          isNamespace: true,
          isStarReexport: false,
          isReexport: true,
          line,
        });
      }
    } else {
      bindings.push({
        localName: "*",
        importedName: "*",
        moduleSpecifier: fromSpec,
        isTypeOnly: /^export\s+type\s+\*/.test(text),
        isNamespace: false,
        isStarReexport: true,
        isReexport: true,
        line,
      });
    }
  }

  // export { a, b as c } from './mod'
  if (fromSpec) {
    for (const c of node.descendantsOfType("export_specifier")) {
      if (!c) continue;
      const nameNode =
        c.childForFieldName("name") ??
        namedChildren(c).find((x) => x.type === "identifier");
      const aliasNode =
        c.childForFieldName("alias") ??
        namedChildren(c).filter((x) => x.type === "identifier")[1];
      if (!nameNode) continue;
      const importedName = nameNode.text;
      const localName = aliasNode ? aliasNode.text : importedName;
      bindings.push({
        localName,
        importedName,
        moduleSpecifier: fromSpec,
        isTypeOnly: false,
        isNamespace: false,
        isStarReexport: false,
        isReexport: true,
        line: c.startPosition.row + 1,
      });
      exportedNames.add(localName);
    }
  }

  // export default function/class/expr
  const isDefault = node.children.some((c) => c?.type === "default");
  if (isDefault && !enclosing) {
    for (const c of namedChildren(node)) {
      if (
        c.type === "function_declaration" ||
        c.type === "class_declaration" ||
        c.type === "abstract_class_declaration"
      ) {
        const name = childText(c, "name") ?? "default";
        if (name !== "default" && !symbols.some((s) => s.name === "default")) {
          symbols.push({
            kind: c.type.includes("class") ? "class" : "function",
            name: "default",
            qualifiedName: "default",
            startLine: c.startPosition.row + 1,
            endLine: c.endPosition.row + 1,
            exported: true,
            signature: firstLine(source, c),
          });
        }
      } else if (
        c.type === "arrow_function" ||
        c.type === "function_expression" ||
        c.type === "identifier" ||
        c.type === "object" ||
        c.type === "class"
      ) {
        if (!symbols.some((s) => s.name === "default")) {
          symbols.push({
            kind:
              c.type === "arrow_function" || c.type === "function_expression"
                ? "function"
                : c.type.includes("class")
                  ? "class"
                  : "variable",
            name: "default",
            qualifiedName: "default",
            startLine: c.startPosition.row + 1,
            endLine: c.endPosition.row + 1,
            exported: true,
            signature: firstLine(source, c),
          });
        }
      }
    }
  }

  // export { foo as bar } (local, no from)
  if (!fromSpec) {
    for (const c of node.descendantsOfType("export_specifier")) {
      if (!c) continue;
      const local = c.childForFieldName("name");
      if (local) exportedNames.add(local.text);
    }
  }
}

function extractHeritage(
  node: Node,
  classQn: string,
  refs: ExtractedRef[],
  forceKind?: "extends" | "implements",
): void {
  const visit = (n: Node) => {
    if (
      n.type === "identifier" ||
      n.type === "type_identifier" ||
      n.type === "member_expression"
    ) {
      const name = n.text;
      const kind: "extends" | "implements" =
        forceKind ??
        (node.type.includes("implements") ? "implements" : "extends");
      refs.push({
        kind,
        srcQualifiedName: classQn,
        rawName: name,
        targetHint: name,
        line: n.startPosition.row + 1,
      });
      return;
    }
    for (const c of namedChildren(n)) visit(c);
  };
  visit(node);
}

function collectExportNames(root: Node): Set<string> {
  const names = new Set<string>();
  const visit = (n: Node) => {
    if (n.type === "export_statement") {
      const isDefault = n.children.some((c) => c?.type === "default");
      if (isDefault) names.add("default");
      for (const c of namedChildren(n)) {
        const nameNode = c.childForFieldName("name");
        if (nameNode) names.add(nameNode.text);
        if (
          c.type === "lexical_declaration" ||
          c.type === "variable_declaration"
        ) {
          for (const d of namedChildren(c)) {
            if (d.type === "variable_declarator") {
              const nm = d.childForFieldName("name");
              if (nm) names.add(nm.text);
            }
          }
        }
      }
      for (const c of n.descendantsOfType("export_specifier")) {
        if (!c) continue;
        const exported =
          c.childForFieldName("alias") ?? c.childForFieldName("name");
        const local = c.childForFieldName("name");
        if (local) names.add(local.text);
        if (exported) names.add(exported.text);
      }
    }
    for (const c of namedChildren(n)) visit(c);
  };
  visit(root);
  return names;
}

function hasExportModifier(node: Node): boolean {
  let p: Node | null = node.parent;
  while (p) {
    if (p.type === "export_statement") return true;
    if (p.type === "program" || p.type === "module") return false;
    p = p.parent;
  }
  const prev = node.previousSibling;
  return prev?.type === "export" || node.text.startsWith("export ");
}

function childText(node: Node, field: string): string | null {
  const c = node.childForFieldName(field);
  return c ? c.text : null;
}

/** Declaration sits directly in the module body (only `export` may wrap it). */
function atModuleLevel(node: Node): boolean {
  let p: Node | null = node.parent;
  while (p) {
    if (p.type === "program" || p.type === "module") return true;
    if (p.type !== "export_statement") return false;
    p = p.parent;
  }
  return false;
}

/**
 * True only for members of a real class/interface. A `method_signature` inside
 * an `object_type` (param type, type alias) is a shape, not a symbol — emitting
 * it invents nodes like `take.run` and poisons unique-name resolution.
 */
function methodOwnerOk(node: Node): boolean {
  let p: Node | null = node.parent;
  while (p) {
    if (p.type === "class_body" || p.type === "interface_body") return true;
    if (p.type === "object_type" || p.type === "statement_block") return false;
    p = p.parent;
  }
  return false;
}

function methodName(node: Node): string | null {
  const name = node.childForFieldName("name");
  if (!name) return null;
  if (
    name.type === "property_identifier" ||
    name.type === "identifier" ||
    name.type === "private_property_identifier"
  ) {
    return name.text;
  }
  if (name.type === "computed_property_name") return null;
  return name.text;
}

function calleeName(node: Node): string | null {
  if (node.type === "identifier") return node.text;
  if (node.type === "member_expression") {
    // keep full M.foo for namespace resolution; also property alone is fallback in resolve
    return node.text;
  }
  if (node.type === "subscript_expression") return null;
  return null;
}

function qualify(enclosing: string | null, name: string): string {
  return enclosing ? `${enclosing}.${name}` : name;
}

function stripQuotes(s: string): string {
  if (
    (s.startsWith('"') && s.endsWith('"')) ||
    (s.startsWith("'") && s.endsWith("'")) ||
    (s.startsWith("`") && s.endsWith("`"))
  ) {
    return s.slice(1, -1);
  }
  return s;
}

function firstLine(source: string, node: Node): string {
  const start = node.startIndex;
  const nl = source.indexOf("\n", start);
  const line = source.slice(start, nl === -1 ? undefined : nl).trim();
  return line.length > 200 ? `${line.slice(0, 200)}…` : line;
}

const BUILTINS = new Set([
  "console",
  "setTimeout",
  "setInterval",
  "clearTimeout",
  "clearInterval",
  "parseInt",
  "parseFloat",
  "isNaN",
  "isFinite",
  "encodeURIComponent",
  "decodeURIComponent",
  "Boolean",
  "Number",
  "String",
  "Array",
  "Object",
  "Promise",
  "Date",
  "Error",
  "Map",
  "Set",
  "JSON",
  "Math",
  "Symbol",
  "BigInt",
  "require",
  "fetch",
  "Buffer",
  "process",
  "structuredClone",
  "queueMicrotask",
]);

function isBuiltin(name: string): boolean {
  const base = name.split(".")[0] ?? name;
  return BUILTINS.has(base);
}
