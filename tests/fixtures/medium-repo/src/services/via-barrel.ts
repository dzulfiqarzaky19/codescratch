import { add as plus } from "@/lib/barrel.js";

export function viaBarrel(a: number, b: number): number {
  return plus(a, b);
}
