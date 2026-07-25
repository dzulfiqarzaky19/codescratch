import { add as sum } from "@/lib/math.js";
import { mul } from "@app/lib/reexport.js";
import { greet } from "@medium/core";

export function total(a: number, b: number): number {
  return sum(a, b) + mul(a, b);
}

export function hello(name: string): string {
  return greet(name);
}
