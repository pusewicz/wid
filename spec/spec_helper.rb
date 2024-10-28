require "minitest/autorun"
require "minitest/focus"
require "minitest/reporters"
require "wid"

Minitest::Reporters.use! Minitest::Reporters::SpecReporter.new
