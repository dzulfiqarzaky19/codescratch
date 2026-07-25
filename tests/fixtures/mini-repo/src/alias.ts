import { add as sum, double as twice } from "./math.js";
import * as MathNs from "./math.js";

export function useAlias(a: number, b: number): number {
  return sum(a, b) + twice(a) + MathNs.mul(a, b);
}
