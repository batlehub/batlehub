terraform {
  required_version = ">= 1.7"

  required_providers {
    # Providers are resolved through the batlehub Terraform registry proxy.
    # Configure the network mirror in .terraformrc (see this directory).
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.65"
    }
    random = {
      source  = "hashicorp/random"
      version = "~> 3.9"
    }
  }
}

provider "aws" {
  region = var.aws_region
}

variable "aws_region" {
  description = "AWS region to deploy into."
  type        = string
  default     = "eu-west-1"
}

resource "random_id" "suffix" {
  byte_length = 4
}

output "suffix" {
  value = random_id.suffix.hex
}
