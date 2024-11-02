guard :minitest do
  watch(%r{^spec/(.*)_spec\.rb$})
  watch(%r{^lib/wid/(.+)\.rb$}) { |m| "spec/#{m[1]}_spec.rb" }
  watch(%r{^spec/spec_helper\.rb$}) { "spec" }
end

guard :rake, task: "nodes" do
  watch(%r{^lib/wid/ast/nodes\.yml$})
end

guard :shell do
  watch(%r{^lib/wid/.*\.rb$}) do |m|
    # Generate RBS for the node
    `bundle exec rbs-inline --output=sig #{m[0]}`
  end
end
