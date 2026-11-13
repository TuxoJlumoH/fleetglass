/** Short kubectl-style age ("45s", "12m", "3h", "2d", "1y") of an RFC 3339 timestamp. */
export function formatAge(timestamp: string, now: number): string {
  const created = Date.parse(timestamp);
  if (Number.isNaN(created)) return timestamp || "—";
  const seconds = Math.max(0, Math.floor((now - created) / 1000));
  if (seconds < 60) return `${String(seconds)}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${String(minutes)}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${String(hours)}h`;
  const days = Math.floor(hours / 24);
  if (days < 365) return `${String(days)}d`;
  return `${String(Math.floor(days / 365))}y`;
}
