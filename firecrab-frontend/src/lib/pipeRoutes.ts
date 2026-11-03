import type { PipeRoute } from "../api/pipe";
export const managedPipeRoute = (route: PipeRoute) =>
  !!(route.owner || route.network);
export const publicPipeRoute = (route: PipeRoute) =>
  !route.network && !route.host.toLowerCase().endsWith(".internal");
/** A deployed MicroService's private `<name>.internal` route. */
export const serviceInternalRoute = (route: PipeRoute) =>
  !!route.network && !!route.owner;
/** Hosts a Firecrab address may publish: operator and domain routes, then service names. */
export const firecrabRouteHosts = (routes: PipeRoute[]) => [
  ...new Set([
    ...routes.filter(publicPipeRoute).map((route) => route.host),
    ...routes.filter(serviceInternalRoute).map((route) => route.host),
  ]),
];
export const editableRouteText = (routes: PipeRoute[]) =>
  routes
    .filter((route) => !managedPipeRoute(route))
    .map(
      (route) =>
        `${route.host} | ${route.pathPrefix} | ${route.upstreams.join(",")}`,
    )
    .join("\n");

/** Preserve controller-owned routes and metadata while replacing operator routes. */
export function parseRouteText(
  text: string,
  existing: PipeRoute[],
): PipeRoute[] {
  const seen = new Set<string>();
  const rows = text
    .split("\n")
    .filter((line) => line.trim())
    .map((line) => {
      const parts = line.split("|").map((value) => value.trim());
      if (parts.length !== 3 || !parts[0] || !parts[1].startsWith("/"))
        throw new Error("Use HOST | /PATH | ADDRESS:PORT on each line.");
      const host = parts[0].toLowerCase(),
        pathPrefix = parts[1],
        key = `${host}|${pathPrefix.replace(/\/+$/, "")}`;
      if (seen.has(key)) throw new Error("This domain and path already exist.");
      seen.add(key);
      const previous = existing.find(
        (route) =>
          route.host === host &&
          route.pathPrefix.replace(/\/+$/, "") ===
            pathPrefix.replace(/\/+$/, ""),
      );
      if (previous && managedPipeRoute(previous))
        throw new Error(
          "Service-managed routes must be changed in MicroService.",
        );
      const upstreams = parts[2]
        .split(",")
        .map((value) => value.trim())
        .filter(Boolean);
      if (!upstreams.length)
        throw new Error("Each route needs at least one app address.");
      for (const address of upstreams) {
        let url: URL;
        try {
          url = new URL(`http://${address}`);
        } catch {
          throw new Error("Enter a valid app address and port.");
        }
        if (
          !url.hostname ||
          !/:\d+$/.test(address) ||
          url.username ||
          url.password ||
          url.pathname !== "/" ||
          url.search ||
          url.hash ||
          Number(address.split(":").at(-1)) < 1 ||
          Number(address.split(":").at(-1)) > 65535
        )
          throw new Error("Enter a valid app address and port.");
      }
      return { ...previous, host, pathPrefix, upstreams };
    });
  let index = 0;
  const result = existing.flatMap((route) =>
    managedPipeRoute(route)
      ? [route]
      : index < rows.length
        ? [rows[index++]]
        : [],
  );
  return [...result, ...rows.slice(index)];
}
