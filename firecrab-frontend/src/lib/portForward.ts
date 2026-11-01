/** Valid TCP/UDP port range; a port forward row needs both ends set. */
export function isValidPort(port: number): boolean {
  return Number.isInteger(port) && port >= 1 && port <= 65535;
}
