import type { PortForward } from "./PortForward";

export type UpdateVmPortForwardsRequest = {
  portForwards: Array<PortForward>;
};
