import type { Page } from "@playwright/test";

/** Use WebIDL 32-bit timer handles when Playwright returns synthetic 1e12 IDs. */
export async function installWasmClock(page: Page): Promise<void> {
  await page.clock.install();
  await page.addInitScript(() =>
    queueMicrotask(() => {
      const timeout = window.setTimeout.bind(window);
      const interval = window.setInterval.bind(window);
      const clearTimeout = window.clearTimeout.bind(window);
      const clearInterval = window.clearInterval.bind(window);
      let next = 0;
      const handles = new Map<number, number>();
      window.setTimeout = ((
        callback: TimerHandler,
        delay?: number,
        ...args: unknown[]
      ) => {
        const id = ++next;
        handles.set(
          id,
          timeout(() => {
            handles.delete(id);
            if (typeof callback === "function") callback(...args);
            else window.eval(callback);
          }, delay),
        );
        return id;
      }) as typeof window.setTimeout;
      window.setInterval = ((
        callback: TimerHandler,
        delay?: number,
        ...args: unknown[]
      ) => {
        const id = ++next;
        handles.set(id, interval(callback, delay, ...args));
        return id;
      }) as typeof window.setInterval;
      window.clearTimeout = ((id?: number) => {
        if (id !== undefined && handles.has(id)) {
          clearTimeout(handles.get(id));
          handles.delete(id);
        }
      }) as typeof window.clearTimeout;
      window.clearInterval = ((id?: number) => {
        if (id !== undefined && handles.has(id)) {
          clearInterval(handles.get(id));
          handles.delete(id);
        }
      }) as typeof window.clearInterval;
    }),
  );
}
