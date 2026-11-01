export type PortProtocol = "tcp" | "udp";

export type PortForward = {
  hostPort: number;
  guestPort: number;
  protocol: PortProtocol;
};
