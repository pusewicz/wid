guard :rake, task: "nodes" do
  watch(%r{^lib/wid/ast/nodes\.yml$})
end

guard :shell do
  watch(%r{^lib/wid/.*\.rb$}) do |m|
    # Generate RBS for the node
    `bundle exec rbs-inline --output=sig #{m[0]}`
  end

  watch(%r{^test/(.*)\.rb$}) { |m| `bin/tldr #{m[0]}` }
  watch(%r{^test/helper\.rb$}) { `bin/tldr` }
  watch(%r{^lib/wid/(.+)\.rb$}) do |m|
    test_file = m[1].split("/").tap do |parts|
      parts << "test_#{parts.pop}.rb"
      parts.unshift("test")
    end.join("/")

    `bin/tldr #{test_file}` if File.exist?(test_file)
  end
end
