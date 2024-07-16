#! /usr/bin/env -S ruby -w
# frozen_string_literal: true

# -*- mode: ruby -*-

CONFIG_FILE = "#{__dir__}/rclone.conf"

# It's okay to share these keys publicly, as they are read-only keys for data
# that we want shared publicly. The data is also available to anyone via HTTPS.
#
DEFAULT_ACCESS_KEY = 'R58TFO04180PSP69L5TB'
DEFAULT_SECRET_KEY = 'W2OUQ3V2BlhgFQdqHqx6Y0N4mdpDBv3M89trAKml'

access_key = ENV['LINODE_OBJ_ACCESS_KEY'] || DEFAULT_ACCESS_KEY
secret_key = ENV['LINODE_OBJ_SECRET_KEY'] || DEFAULT_SECRET_KEY

args = [
  'rclone',
  '-vv', # '--log-level', 'INFO',
  '--config', CONFIG_FILE,
  '--buffer-size=128Mi',
  "--s3-access-key-id=#{access_key}",
  "--s3-secret-access-key=#{secret_key}",
  *ARGV
]
warn "granola-rclone issuing: #{args}"
system(*args) || abort('rclone failed')