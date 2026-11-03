import { useEffect, useState } from "react";

/**
 * Whole seconds since `key` last changed, counted in this browser; null while
 * `key` is null. A reload starts again from 0.
 */
export function useElapsedSeconds(key: string | null): number | null {
  const [elapsed, setElapsed] = useState({ key, seconds: 0 });
  useEffect(() => {
    if (key === null) return;
    const start = Date.now();
    const timer = window.setInterval(
      () =>
        setElapsed({ key, seconds: Math.floor((Date.now() - start) / 1000) }),
      1000,
    );
    return () => {
      window.clearInterval(timer);
      // The same key may start again later; it must count from 0 then.
      setElapsed({ key: null, seconds: 0 });
    };
  }, [key]);
  if (key === null) return null;
  return elapsed.key === key ? elapsed.seconds : 0;
}
