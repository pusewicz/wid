# Rewrites docs/errors example output to match the compiler.
#
# Every ```text block must equal the output of the program state built from
# the ```wid (and ```c) blocks above it. An unlabeled block is `main.wid`; a
# block preceded by a line like "`physics/body.wid`:" is that file. The program
# state after the last block of `## How to fix` must check with no output.
#
# Single-file states run as `check main.wid -file`; multi-file states run as
# `check .` inside the package directory, and as `build` when a .c file exists.
#
# Run it after an intended diagnostic change, review the diff, then confirm
# with scripts/errdocs_drift.rb (which also checks the fix programs).
#
# Pages are read and written as UTF-8 whatever the locale. The C compiler
# defaults to `clang` (set WID_CC to override) because pages that show C
# compiler output, like E0702's, are written against clang, so the results
# don't depend on the machine's `cc`.
#
# Usage: ruby scripts/errdocs_update.rb [CODE...]

require "fileutils"
require "open3"
require "tmpdir"

Encoding.default_external = Encoding::UTF_8
ENV["WID_CC"] ||= "clang"

ROOT = File.expand_path("..", __dir__)
WID = ENV.fetch("WID_BIN", "#{ROOT}/target/debug/wid")
WORK = ENV["WID_WORK"] || Dir.mktmpdir("wid-errdocs")
TRAILER = /\A(error: could not compile due to|warning: \d+ warnings? emitted)/

Block = Struct.new(:lang, :label, :body, :section, :line)

# Parses a page into fenced blocks, each tagged with its file label and section.
def parse(text)
  out = []
  cur = nil
  section = nil
  last_text = nil
  text.each_line(chomp: true).with_index(1) do |line, n|
    if cur
      if line == "```"
        out << cur
        cur = nil
      else
        cur.body << line
      end
    elsif line.start_with?("```")
      label = last_text && last_text[/\A`([\w.\/-]+\.(?:wid|c|h))`[^`]*:\z/, 1]
      cur = Block.new(line.delete_prefix("```"), label, [], section, n)
    else
      section = line.delete_prefix("## ") if line.start_with?("## ")
      last_text = line unless line.strip.empty?
    end
  end
  out
end

# Runs the compiler on a file set; returns the output lines without trailers.
def run(files, tag, flags = [])
  dir = File.join(WORK, tag)
  FileUtils.rm_rf(dir)
  files.each do |path, body|
    FileUtils.mkdir_p(File.dirname(File.join(dir, path)))
    File.write(File.join(dir, path), body.join("\n") + "\n")
  end
  env = { "WID_ROOT" => ROOT, "NO_COLOR" => "1" }
  args =
    if files.keys == ["main.wid"] then ["check", "main.wid", "-file"]
    elsif files.keys.any? { |k| k.end_with?(".c") } then ["build", "."]
    else ["check", "."]
    end
  out, = Open3.capture2e(env, WID, *args, *flags, chdir: dir)
  lines = out.lines.map(&:chomp).reject { |l| l =~ TRAILER }
  lines.pop while lines.last == ""
  lines
end

# Rewrites every ```text block that differs from the compiler output.
codes = ARGV.empty? ? Dir["#{ROOT}/docs/errors/*.md"].map { |f| File.basename(f, ".md") }.sort : ARGV
codes.each do |code|
  path = "#{ROOT}/docs/errors/#{code}.md"
  lines = File.readlines(path, chomp: true)
  page = lines.join("\n") + "\n"
  # `<!-- drift: skip REASON -->` marks a page whose output no `check` run reproduces.
  if (why = page[/<!-- drift: skip (.*?) -->/, 1])
    puts "#{code}: skipped (#{why})"
    next
  end
  flags = page[/<!-- flags: (.*?) -->/, 1].to_s.split
  blocks = parse(page)
  files = {}
  prev_section = nil
  replacements = []
  blocks.each_with_index do |b, i|
    files = {} if b.section != prev_section && b.section != "How to fix" && %w[wid c].include?(b.lang)
    prev_section = b.section if %w[wid c text].include?(b.lang)
    case b.lang
    when "wid", "c" then files[b.label || "main.wid"] = b.body
    when "text"
      out = run(files, "#{code}-#{i}", flags)
      replacements << [b.line, b.body.size, out] if out != b.body
    end
  end
  next if replacements.empty?
  replacements.reverse_each { |line, n, out| lines[line, n] = out }
  File.write(path, lines.join("\n") + "\n")
  puts "#{code}: updated #{replacements.size} block(s)"
end
