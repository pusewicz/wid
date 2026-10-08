# Verifies docs/errors pages against the snapshot compiler.
#
# Every ```text block must equal the output of the program state built from
# the ```wid (and ```c) blocks above it. An unlabeled block is `main.wid`; a
# block preceded by a line like "`physics/body.wid`:" is that file. The program
# state after the last block of `## How to fix` must check with no output.
#
# Single-file states run as `check main.wid -file`; multi-file states run as
# `check .` inside the package directory, and as `build` when a .c file exists.
# A page about another command says which with `<!-- command: ARGS -->`, like
# `<!-- command: doc . Ball.sped -->`: its examples run as `wid ARGS` in the
# package directory, and `<!-- fix-command: ARGS -->` is its fix, which must
# succeed with nothing on stderr.
#
# Pages are read as UTF-8 whatever the locale. The C compiler defaults to
# `clang` (set WID_CC to override) because pages that show C compiler output,
# like E0702's, are written against clang, so the results don't depend on
# the machine's `cc`.
#
# Usage: ruby scripts/errdocs_drift.rb [-v] [CODE...]

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

# Writes a file set into a fresh directory named after `tag`.
def write_files(files, tag)
  dir = File.join(WORK, tag)
  FileUtils.rm_rf(dir)
  FileUtils.mkdir_p(dir)
  files.each do |path, body|
    FileUtils.mkdir_p(File.dirname(File.join(dir, path)))
    File.write(File.join(dir, path), body.join("\n") + "\n")
  end
  dir
end

ENV_VARS = { "WID_ROOT" => ROOT, "NO_COLOR" => "1" }.freeze

# Runs the compiler on a file set; returns the output lines without trailers.
def run(files, tag, flags = [], command = nil)
  dir = write_files(files, tag)
  args =
    if command then command
    elsif files.keys == ["main.wid"] then ["check", "main.wid", "-file"]
    elsif files.keys.any? { |k| k.end_with?(".c") } then ["build", "."]
    else ["check", "."]
    end
  out, = Open3.capture2e(ENV_VARS, WID, *args, *flags, chdir: dir)
  lines = out.lines.map(&:chomp).reject { |l| l =~ TRAILER }
  lines.pop while lines.last == ""
  lines
end

verbose = ARGV.delete("-v")
codes = ARGV.empty? ? Dir["#{ROOT}/docs/errors/*.md"].map { |f| File.basename(f, ".md") }.sort : ARGV
bad = 0
codes.each do |code|
  page = File.read("#{ROOT}/docs/errors/#{code}.md")
  # `<!-- drift: skip REASON -->` marks a page whose output no `check` run reproduces.
  if (why = page[/<!-- drift: skip (.*?) -->/, 1])
    puts "#{code}: skipped (#{why})"
    next
  end
  # `<!-- flags: ... -->` gives the example (not the fix) compiler flags.
  flags = page[/<!-- flags: (.*?) -->/, 1].to_s.split
  command = page[/<!-- command: (.*?) -->/, 1]&.split
  fix_command = page[/<!-- fix-command: (.*?) -->/, 1]&.split
  blocks = parse(page)
  files = {}
  pending = false
  prev_section = nil
  fix_seen = false
  report = []
  check_fix = lambda do
    out = run(files, "#{code}-fix")
    if out.empty?
      report << "fix OK"
    else
      report << "fix FAIL"
      out.each { |l| report << "    | #{l}" } if verbose
    end
  end
  blocks.each_with_index do |b, i|
    if fix_seen && b.section != "How to fix" && pending
      check_fix.call
      pending = false
      fix_seen = false
    end
    files = {} if b.section != prev_section && b.section != "How to fix" && %w[wid c].include?(b.lang)
    prev_section = b.section if %w[wid c text].include?(b.lang)
    case b.lang
    when "wid", "c"
      files[b.label || "main.wid"] = b.body
      pending = true
      fix_seen = true if b.section == "How to fix"
    when "text"
      out = run(files, "#{code}-#{i}", flags, command)
      if out == b.body
        report << "text@#{b.line} OK"
      else
        report << "text@#{b.line} DIFF"
        if verbose
          report << "  --- page"
          b.body.each { |l| report << "    | #{l}" }
          report << "  --- actual"
          out.each { |l| report << "    | #{l}" }
        end
      end
      pending = false
    end
  end
  if fix_command
    dir = write_files(files, "#{code}-fix")
    out, err, status = Open3.capture3(ENV_VARS, WID, *fix_command, chdir: dir)
    if status.success? && err.empty?
      report << "fix OK"
    else
      report << "fix FAIL"
      (err + out).lines.each { |l| report << "    | #{l.chomp}" } if verbose
    end
  elsif fix_seen && pending
    check_fix.call
  elsif !blocks.any? { |b| b.section == "How to fix" && b.lang == "wid" }
    report << "no fix program"
  end
  bad += 1 if report.any? { |r| r =~ /DIFF|FAIL|no fix/ }
  puts "#{code}: #{report.reject { |r| r.start_with?(" ") }.join(', ')}"
  report.select { |r| r.start_with?(" ") }.each { |r| puts r }
end
puts "#{bad} page(s) with problems" if codes.size > 1
exit(bad.zero? ? 0 : 1)
