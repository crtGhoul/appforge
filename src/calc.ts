/**
 * Tiny expression evaluator for the launcher calculator (=2+2).
 *
 * Recursive-descent parser with explicit precedence:
 *   expression := term (("+" | "-") term)*
 *   term       := factor (("*" | "/" | "%") factor)*
 *   factor     := ("-" | "+") factor | power   (unary binds looser than ^,
 *                                               so -2^2 reads as -(2^2))
 *   power      := primary ("^" factor)?        (right-associative)
 *   primary    := number | "(" expression ")"
 *   number     := digits[.digits] | .digits
 *
 * NEVER uses eval or the Function constructor — the input only ever
 * flows through this parser. Returns null for anything that isn't a
 * valid, finite calculation.
 */

class Parser {
  private i = 0;

  constructor(private readonly s: string) {}

  private peek(): string {
    return this.s[this.i] ?? "";
  }

  private skipWs(): void {
    while (/\s/.test(this.peek())) this.i++;
  }

  parseExpression(): number {
    let value = this.parseTerm();
    for (;;) {
      this.skipWs();
      const c = this.peek();
      if (c === "+" || c === "-") {
        this.i++;
        const rhs = this.parseTerm();
        value = c === "+" ? value + rhs : value - rhs;
      } else {
        return value;
      }
    }
  }

  private parseTerm(): number {
    let value = this.parseFactor();
    for (;;) {
      this.skipWs();
      const c = this.peek();
      if (c === "*" || c === "/" || c === "%") {
        this.i++;
        const rhs = this.parseFactor();
        value = c === "*" ? value * rhs : c === "/" ? value / rhs : value % rhs;
      } else {
        return value;
      }
    }
  }

  private parseFactor(): number {
    // Unary minus binds looser than ^, so -2^2 reads as -(2^2).
    this.skipWs();
    const c = this.peek();
    if (c === "-") {
      this.i++;
      return -this.parseFactor();
    }
    if (c === "+") {
      this.i++;
      return this.parseFactor();
    }
    return this.parsePower();
  }

  private parsePower(): number {
    const base = this.parsePrimary();
    this.skipWs();
    if (this.peek() === "^") {
      this.i++;
      return Math.pow(base, this.parseFactor());
    }
    return base;
  }

  private parsePrimary(): number {
    this.skipWs();
    if (this.peek() === "(") {
      this.i++;
      const value = this.parseExpression();
      this.skipWs();
      if (this.peek() !== ")") throw new Error("missing )");
      this.i++;
      return value;
    }
    return this.parseNumber();
  }

  private parseNumber(): number {
    this.skipWs();
    const rest = this.s.slice(this.i);
    const m = /^\d+(\.\d+)?/.exec(rest) ?? /^\.\d+/.exec(rest);
    if (!m) throw new Error("expected a number");
    this.i += m[0].length;
    return parseFloat(m[0]);
  }

  expectEnd(): void {
    this.skipWs();
    if (this.i < this.s.length) throw new Error("trailing input");
  }
}

/**
 * Evaluate an arithmetic expression. Returns the numeric result, or null
 * when the input isn't a valid calculation (syntax error, division that
 * yields a non-finite result, empty input, ...).
 */
export function evaluateExpression(input: string): number | null {
  const s = input.trim();
  if (!s) return null;
  try {
    const parser = new Parser(s);
    const value = parser.parseExpression();
    parser.expectEnd();
    if (!Number.isFinite(value)) return null;
    return value;
  } catch {
    return null;
  }
}

/**
 * Format a calculator result for display and clipboard copy. Rounds away
 * float noise (0.1+0.2 shows as 0.3, not 0.30000000000000004).
 */
export function formatCalcResult(value: number): string {
  const rounded = Math.round(value * 1e10) / 1e10;
  return String(rounded);
}
