# Runs each program in a probe file through `wid check main.wid -file`.
# Programs are separated by lines starting with `### ` followed by a name.
#
# Usage: ruby scripts/probe.rb FILE

require "fileutils"
require "open3"
require "tmpdir"

ROOT = File.expand_path("..", __dir__)
WID = ENV.fetch("WID_BIN", "#{ROOT}/target/debug/wid")
WORK = ENV["WID_WORK"] || Dir.mktmpdir("wid-errdocs")

progs = File.read(ARGV.fetch(0)).split(/^### /).reject(&:empty?)
progs.each do |chunk|
  name, src = chunk.split("\n", 2)
  dir = "#{WORK}/#{name.strip.gsub(/\W/, '_')}"
  FileUtils.rm_rf(dir)
  FileUtils.mkdir_p(dir)
  File.write("#{dir}/main.wid", src.sub(/\n+\z/, "\n"))
  out, st = Open3.capture2e({ "WID_ROOT" => ROOT, "NO_COLOR" => "1" }, WID, "check", "main.wid", "-file", chdir: dir)
  puts "=== #{name.strip} (exit #{st.exitstatus})"
  puts out.lines.reject { |l| l.start_with?("error: could not compile") }.join.rstrip
end
