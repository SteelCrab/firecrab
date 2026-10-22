import { useLayoutEffect, useState } from "react";
import type { RefObject } from "react";

/** Border-box width of `ref`'s element, kept current while it resizes; 0 before it mounts. */
export function useElementWidth(ref: RefObject<HTMLElement | null>): number {
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    setWidth(element.getBoundingClientRect().width);
    const observer = new ResizeObserver(() => setWidth(element.getBoundingClientRect().width));
    observer.observe(element);
    return () => observer.disconnect();
  }, [ref]);
  return width;
}
