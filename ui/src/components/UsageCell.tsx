import type { UsageCell as UsageCellData } from "../generated/bindings";

export interface UsageCellProps {
  cell?: UsageCellData | null | undefined;
}

export function UsageCell({ cell }: UsageCellProps) {
  if (!cell || !cell.formatted || cell.formatted === "—") {
    return <span className="use-empty">—</span>;
  }

  const { value, formatted, request, limit } = cell;

  const hasLimit = limit != null && limit > 0;
  const hasRequest = request != null && request > 0;

  let subtext = "";
  let colorClass = "";
  let mainPercent = 0;
  let reqPercent: number | null = null;

  if (hasLimit) {
    mainPercent = (value / limit) * 100;
    const rounded = Math.round(mainPercent);
    subtext = `${String(rounded)}% lim`;
    if (mainPercent > 100) {
      colorClass = "err";
    } else if (mainPercent >= 80) {
      colorClass = "warn";
    }
    if (hasRequest) {
      reqPercent = Math.min(100, Math.max(0, Math.round((request / limit) * 100)));
    }
  } else if (hasRequest) {
    mainPercent = (value / request) * 100;
    const rounded = Math.round(mainPercent);
    subtext = `${String(rounded)}% req`;
    reqPercent = 100;
  }

  const mainWidth = Math.min(100, Math.max(0, Math.round(mainPercent)));

  return (
    <div className="use">
      <div className="v">
        {formatted}
        <span>{subtext}</span>
      </div>
      <div className="bar">
        {reqPercent !== null && (
          <i className="req" style={{ width: `${String(reqPercent)}%` }} />
        )}
        <i className={colorClass} style={{ width: `${String(mainWidth)}%` }} />
      </div>
    </div>
  );
}
