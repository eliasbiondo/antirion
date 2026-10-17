/** How wide a count drawn next to an annotation flag is, at the flag's 10px mono type. */
const DIGIT_PX = 6.1;
/** Space between a flag and its count, and from the count to the next flag. */
const GAP_PX = 3;
/** Half the drawn width of a flag (a 7px square turned 45°). */
const FLAG_HALF_PX = 5;

/**
 * Which annotation flags may show their count beside them: a count is drawn only for two or more
 * events and only where it ends before the next flag and inside the plot, so counts never
 * collide; an elided count is still in the flag's tooltip and the hover card.
 */
export function noteCountFits(xs: readonly number[], counts: readonly number[], right: number): boolean[] {
  return xs.map((x, i) => {
    const count = counts[i] ?? 0;
    if (count < 2) return false;
    const end = x + FLAG_HALF_PX + GAP_PX + String(count).length * DIGIT_PX;
    const next = xs[i + 1];
    const limit = next === undefined ? right : Math.min(right, next - FLAG_HALF_PX - GAP_PX);
    return end <= limit;
  });
}

/** The tooltip of one flag: its events, the first few by name. */
export function noteTitle(labels: readonly string[]): string {
  const shown = labels.slice(0, 3).join('\n');
  const more = labels.length - 3;
  return more > 0 ? `${shown}\nand ${more} more` : shown;
}
