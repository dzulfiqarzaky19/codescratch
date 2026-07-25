import { add, double } from "./math.js";

export class Calculator {
  sum(a: number, b: number): number {
    return add(a, b);
  }

  twice(x: number): number {
    return double(x);
  }
}

export function run(): number {
  const c = new Calculator();
  return c.sum(1, 2) + c.twice(3);
}
