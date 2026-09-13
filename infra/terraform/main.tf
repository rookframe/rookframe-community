terraform {
  required_version = ">= 1.6.0"
  required_providers {
    hcloud = {
      source  = "hetznercloud/hcloud"
      version = ">= 1.65.0, < 2.0.0"
    }
  }
}
provider "hcloud" {}
variable "name" { default = "rookframe-community" }
variable "location" { default = "nbg1" }
variable "server_type" { default = "cx23" }
variable "ssh_public_key_path" { default = "~/.ssh/id_ed25519.pub" }
variable "existing_ssh_key_name" {
  type        = string
  default     = null
  description = "Reuse a project key when its public key is already registered."
}
variable "admin_cidrs" {
  type = list(string)
  validation {
    condition     = length(var.admin_cidrs) > 0 && alltrue([for cidr in var.admin_cidrs : can(cidrnetmask(cidr)) && !endswith(cidr, "/0")])
    error_message = "Supply restricted IPv4 CIDRs for administration."
  }
}
locals { labels = { app = "rookframe-community", managed_by = "opentofu" } }
data "hcloud_ssh_key" "existing" {
  count = var.existing_ssh_key_name == null ? 0 : 1
  name  = var.existing_ssh_key_name
}
resource "hcloud_ssh_key" "admin" {
  count      = var.existing_ssh_key_name == null ? 1 : 0
  name       = "${var.name}-admin"
  public_key = file(pathexpand(var.ssh_public_key_path))
  labels     = local.labels
}
resource "hcloud_firewall" "community" {
  name   = "${var.name}-firewall"
  labels = local.labels
  rule {
    direction  = "in"
    protocol   = "tcp"
    port       = "22"
    source_ips = var.admin_cidrs
  }
  rule {
    direction  = "in"
    protocol   = "tcp"
    port       = "80"
    source_ips = ["0.0.0.0/0", "::/0"]
  }
  rule {
    direction  = "in"
    protocol   = "tcp"
    port       = "443"
    source_ips = ["0.0.0.0/0", "::/0"]
  }
}
resource "hcloud_server" "community" {
  name         = var.name
  image        = "ubuntu-24.04"
  location     = var.location
  server_type  = var.server_type
  ssh_keys     = var.existing_ssh_key_name == null ? [hcloud_ssh_key.admin[0].id] : [data.hcloud_ssh_key.existing[0].id]
  firewall_ids = [hcloud_firewall.community.id]
  backups      = true
  labels       = local.labels
  user_data    = file("${path.module}/cloud-init.yml")
}
resource "hcloud_volume" "data" {
  name      = "${var.name}-data"
  size      = 10
  server_id = hcloud_server.community.id
  automount = true
  format    = "ext4"
  labels    = local.labels
  lifecycle { prevent_destroy = true }
}
output "server_ipv4" { value = hcloud_server.community.ipv4_address }
output "server_id" { value = hcloud_server.community.id }
output "data_path" { value = "/mnt/HC_Volume_${hcloud_volume.data.id}" }
