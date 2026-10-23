variable "api_url" {
  type        = string
  description = "Trusted Firecrab API URL, usually a loopback tunnel."
}

variable "name" {
  type        = string
  description = "Unique prefix for disposable resources."
  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{0,47}$", var.name))
    error_message = "Use a lowercase DNS label with at most 48 characters."
  }
}

variable "subnet_cidr" {
  type        = string
  description = "Unused IPv4 network that does not overlap an existing network or host route."
}

variable "template" {
  type        = string
  description = "Already installed whoami VM template, with a minimum disk size of at most 1 GiB."
}

variable "host_port" {
  type        = number
  description = "Unused host TCP port forwarded to the VM's HTTP port 80."
  validation {
    condition     = var.host_port >= 1024 && var.host_port <= 65535 && floor(var.host_port) == var.host_port
    error_message = "Choose an integer port from 1024 through 65535."
  }
}

variable "service_image" {
  type        = string
  description = "Real OCI image reference pinned by SHA-256 digest. This example saves configuration only."
  validation {
    condition     = can(regex("^.+@sha256:[0-9a-f]{64}$", var.service_image))
    error_message = "Provide a digest-pinned image reference."
  }
}

variable "vm_egress_policy" {
  type        = string
  default     = "internet"
  description = "Explicit VM egress posture, preserved in creation and update payloads."
  validation {
    condition     = contains(["internet", "isolated"], var.vm_egress_policy)
    error_message = "Use internet or isolated."
  }
}

variable "vm_security_group_id" {
  type        = string
  default     = null
  description = "Existing security group UUID for this network; null explicitly selects legacy networking."
}

variable "service_domain" {
  type        = string
  default     = null
  description = "Managed service domain; null explicitly disables domain routing."
}

variable "service_replicas" {
  type        = number
  default     = 1
  description = "Managed service replica count."
}

variable "service_env" {
  type        = map(string)
  default     = {}
  description = "Managed plaintext test environment values, also stored in Terraform state."
}

variable "service_security_group_id" {
  type        = string
  default     = null
  description = "Existing service security group UUID; null explicitly selects legacy networking."
}
