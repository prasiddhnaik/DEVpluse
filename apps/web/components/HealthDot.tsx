import { healthStyle } from "@/lib/format";
import type { Health } from "@/lib/types";

export function HealthDot({ health, className = "" }: { health: Health; className?: string }) {
  const style = healthStyle[health];
  return (
    <span
      className={`inline-flex size-2.5 rounded-full ${style.dot} ${className}`}
      title={style.label}
      aria-label={style.label}
    />
  );
}
