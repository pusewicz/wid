# frozen_string_literal: true

require "rake/clean"
require "yaml"
require "standard/rake"
require "minitest/test_task"

NODES = YAML.load_file("lib/wid/ast/nodes.yml")
FILES = NODES["nodes"].map do |node|
  name = node["name"].gsub(/([A-Z]+)([A-Z][a-z])/, '\1_\2')
    .gsub(/([a-z\d])([A-Z])/, '\1_\2')
    .tr("-", "_")
    .downcase
  "lib/wid/ast/node/#{name}.rb"
end

CLEAN.include(FILES)

rule(/lib\/wid\/ast\/node\/.*\.rb$/ => "lib/wid/ast/nodes.yml") do |t|
  require "erb"
  require "ostruct"
  node_name = t.name.match(/node\/(.*)\.rb/)[1].split("_").map(&:capitalize).join
  file_name = t.name.match(/node\/(.*)\.rb/)[1]
  erb = ERB.new File.read("lib/wid/ast/node/node.rb.erb"), trim_mode: "-"

  node_struct = Data.define(:name, :human_name, :fields)
  field_struct = Data.define(:_name, :type, :kind) do
    def name
      _name.sub(/\?$/, "")
    end

    def list?
      type&.end_with? "[]"
    end

    def node?
      kind&.end_with? "Node"
    end

    def rbs_type
      if list?
        "Array[#{kind}]"
      else
        kind.to_s
      end
    end

    def emit_to_h
      return "" unless node?

      if list?
        ".map(&:to_h)"
      else
        ".to_h"
      end
    end
  end

  NODES["nodes"].find { |n| n["name"] == node_name }.tap do |node|
    node = node_struct.new(
      file_name,
      node_name,
      node["fields"]&.map { |f| field_struct.new(f["name"], f["type"], f["kind"]) } || []
    )
    puts "Generating Wid::AST::Node::#{node_name}..."
    File.binwrite t.name, erb.result(binding)
  end
end

task nodes: FILES
task spec: FILES

task default: [:spec, "standard:fix"]

Minitest::TestTask.create :spec do |t|
  t.test_globs = ["spec/**/*_spec.rb"]
end
