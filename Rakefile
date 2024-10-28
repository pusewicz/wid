# frozen_string_literal: true

require "standard/rake"
require "minitest/test_task"
require "minitest/fail_fast" if ENV["TESTOPTS"] == "--fail-fast"

task default: [:spec, "standard:fix"]

Minitest::TestTask.create :spec do |t|
  t.test_globs = ["spec/**/*_spec.rb"]
end
