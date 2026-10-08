# Compiles the generated C of every `tests/run` program at every `-o:` level
# with each C compiler, under `-std=c23 -Wall -Wextra -Wpedantic -Werror`, and
# lists the programs that don't compile cleanly. The suite builds each
# program at one level (its `NAME.flags`, or `-o:minimal`); gcc warns about
# more at -O2 and -O3 (#130), so run this after changing the runtime or the
# emitter.
#
# Each program is built once with `wid build -keep-c` (with its own flags,
# but no `-o:`), then its C is compiled again with `-c` at each level. A
# package's subdirectories go on the include path, for the headers its
# `cimport`s name.
#
# Usage: ruby scripts/opt_levels.rb [-O0,-O1,-Os,-O2,-O3] [clang,gcc-15]
#
# The compilers default to clang and the newest gcc found (gcc-16, gcc-15).
# WID_FLAGS adds `wid build` flags to every program, like `WID_FLAGS=-debug`
# for the C of debug builds.

require "etc"
require "fileutils"
require "open3"
require "tmpdir"

ROOT = File.expand_path("..", __dir__)
WID = ENV.fetch("WID_BIN", "#{ROOT}/target/debug/wid")
WORK = ENV["WID_WORK"] || Dir.mktmpdir("wid-opt-levels")
STRICT = %w[-std=c23 -Wall -Wextra -Wpedantic -Werror -fwrapv -fno-strict-aliasing].freeze

def found?(cc)
  _, status = Open3.capture2e(cc, "--version")
  status.success?
rescue SystemCallError
  false
end

levels = (ARGV[0] || "-O0,-O1,-Os,-O2,-O3").split(",")
compilers = ARGV[1]&.split(",") || ["clang", %w[gcc-16 gcc-15].find { |cc| found?(cc) }].compact

cases = Dir.children("#{ROOT}/tests/run").sort.filter_map do |entry|
  path = "#{ROOT}/tests/run/#{entry}"
  if File.directory?(path) then [entry, path, false]
  elsif entry.end_with?(".wid") then [entry.delete_suffix(".wid"), path, true]
  end
end

queue = Queue.new
cases.each { |c| queue << c }
failures = Queue.new
workers = Array.new(Etc.nprocessors) do
  Thread.new do
    while (job = (queue.pop(true) rescue nil))
      name, path, file = job
      base = file ? path.delete_suffix(".wid") : path
      flags = File.exist?("#{base}.flags") ? File.read("#{base}.flags").split : []
      flags -= flags.grep(/\A-o:/)
      flags |= ENV.fetch("WID_FLAGS", "").split
      dir = "#{WORK}/#{name}"
      FileUtils.mkdir_p(dir)
      args = [WID, "build", path, *(file ? ["-file"] : []), *flags, "-keep-c", "-cc:#{compilers.first}", "-out:#{dir}/#{name}"]
      out, status = Open3.capture2e({ "WID_ROOT" => ROOT, "NO_COLOR" => "1" }, *args)
      unless status.success?
        failures << "#{name}: `wid build` failed\n#{out}"
        next
      end
      includes = file ? [] : Dir.glob("#{path}/**/").flat_map { |d| ["-I", d] }
      debug = flags.include?("-debug") ? ["-g"] : []
      compilers.each do |cc|
        levels.each do |level|
          cmd = [cc, *STRICT, level, *debug, "-I", dir, *includes, "-c", "#{dir}/#{name}.c", "-o", File::NULL]
          out, status = Open3.capture2e(*cmd)
          next if status.success?
          failures << "#{name} (#{cc} #{level}):\n#{out.lines.grep(/error|warning/).first(8).join}"
        end
      end
    end
  end
end
workers.each(&:join)

list = []
list << failures.pop until failures.empty?
puts list.sort
puts "#{cases.size} programs, #{list.size} failures (#{compilers.join(', ')} at #{levels.join(', ')})"
FileUtils.rm_rf(WORK) unless ENV["WID_WORK"]
exit(list.empty? ? 0 : 1)
