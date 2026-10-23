terraform {
  required_version = ">= 1.10.0"
  required_providers {
    restapi = {
      source  = "Mastercard/restapi"
      version = "3.0.0"
    }
  }
}

provider "restapi" {
  uri                  = var.api_url
  write_returns_object = true
  timeout              = 60
}

resource "restapi_object" "network" {
  path      = "/api/micro-networks"
  read_path = "/api/micro-networks"
  read_search = {
    search_key   = "id"
    search_value = "{id}"
  }
  data = jsonencode({
    name            = "${var.name}-net"
    subnetCidr      = var.subnet_cidr
    internetEnabled = true
  })
  update_method = "PATCH"
  update_data   = jsonencode({ internetEnabled = true })
  force_new     = ["name", "subnetCidr"]
  # In restapi 3.0.0 ignore_server_additions preserves stale desired data on refresh.
  # Ignore specific response-only fields instead, so declared field drift is visible.
  ignore_changes_to = ["id", "gateway", "uplink", "ipv6Cidr", "ipv6Gateway", "ipv6AddressMode", "ipv6Egress"]
}

resource "restapi_object" "vm" {
  path = "/api/vms"
  data = jsonencode({
    name           = "${var.name}-vm"
    template       = var.template
    cpu            = 1
    ram            = 256
    diskGb         = 1
    microNetworkId = restapi_object.network.id
    env            = { WHOAMI_NAME = var.name }
    portForwards   = [{ hostPort = var.host_port, guestPort = 80, protocol = "tcp" }]
  })
  update_data = jsonencode({
    cpu = 1, ram = 256, diskGb = 1, env = { WHOAMI_NAME = var.name }
  })
  # This example manages stopped VM configuration. Changes replace the VM.
  force_new = ["name", "template", "cpu", "ram", "diskGb", "microNetworkId", "env", "portForwards"]
  ignore_changes_to = [
    "id", "purpose", "state", "reconciliation", "templateVersion", "startupStep", "egressPolicy", "securityGroupId",
    "ipv4", "ipv6", "mac", "hostname", "startupTimeline", "storageRoot", "shellRefs",
    "cpuUsagePercent", "memoryUsedMib", "memoryTotalMib", "memoryUsedPercent", "usageHistory", "sshHostFingerprint"
  ]
}

resource "restapi_object" "service" {
  path = "/api/micro-services"
  data = jsonencode({
    name       = "${var.name}-svc"
    image      = var.service_image
    port       = 80
    healthPath = "/"
    ram        = 256
    cpu        = 1
  })
  ignore_changes_to = ["id", "runtime", "domain", "replicas", "startupTimeout", "drainTimeout", "env", "secrets", "securityGroupId"]
}

output "network_id" { value = restapi_object.network.id }
output "vm_id" { value = restapi_object.vm.id }
output "service_id" { value = restapi_object.service.id }
