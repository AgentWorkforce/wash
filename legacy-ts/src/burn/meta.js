// Helpers for building the `_meta` annotation that every relaywash tool result carries.
// Burn's annotation reader (AgentWorkforce/burn#219) reads this to attribute savings.

/**
 * @param {string[]} replaces  Built-in tool names this call collapses (e.g. ['Glob','Grep','Read']).
 * @param {number} collapsedCalls  Estimated number of vanilla tool calls this single call replaced.
 */
export function meta(replaces, collapsedCalls) {
  return { replaces, collapsedCalls };
}
