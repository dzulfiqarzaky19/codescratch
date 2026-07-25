import { sep } from "node:path";

export function toPosix(p: string): string {
  return p.split(sep).join("/").replace(/\\/g, "/");
}

export function extnamePosix(p: string): string {
  const i = p.lastIndexOf(".");
  const s = p.lastIndexOf("/");
  if (i <= s) return "";
  return p.slice(i);
}
