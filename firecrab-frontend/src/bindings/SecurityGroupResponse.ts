/** Allow-only IPv4 policy shared with firecrab-api-types. */
export type SecurityGroupRule = {
  direction: "ingress" | "egress";
  protocol: "tcp" | "udp";
  portFrom: number;
  portTo: number;
  peerCidr: string;
};

export type SecurityGroupResponse = {
  id: string;
  name: string;
  microNetworkId: string;
  rules: SecurityGroupRule[];
};

export type CreateSecurityGroupRequest = { name: string; microNetworkId: string };
export type ReplaceSecurityGroupRulesRequest = { rules: SecurityGroupRule[] };
