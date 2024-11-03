# frozen_string_literal: true

require "rake/clean"
require "standard/rake"
require "tldr/rake"

CLEAN.include("lib/wid/ast/node.rb")
file "lib/wid/ast/node.rb" => "lib/wid/ast/node.rb.erb" do |t|
  require "erb"
  require "ostruct"
  require "yaml"

  nodes = YAML.load_file("lib/wid/ast/nodes.yml")["nodes"]
  erb = ERB.new File.read(t.source), trim_mode: "-"
  node_struct = Data.define(:name, :class_name, :fields)
  field_struct = Data.define(:name, :type, :kind) do
    def list?
      type&.end_with? "[]"
    end

    def nilable?
      type&.end_with? "?"
    end

    def node?
      type&.start_with? "node"
    end

    def rbs_type
      if list?
        "Array[#{kind}]"
      else
        Array(kind).map(&:to_s).tap do |v|
          v << "nil" if nilable?
        end.join(" | ")
      end
    end

    def emit_to_h
      return "" unless node?

      if list?
        ".map(&:to_h)"
      else
        nilable? ? "&.to_h" : ".to_h"
      end
    end
  end
  nodes = nodes.map do |node|
    # Turns class names into underscore using plain ruby
    node_struct.new(
      name: node["name"].gsub(/([A-Z])/, '_\1').downcase.sub(/^_/, ""),
      class_name: node["name"],
      fields: node["fields"]&.map { |f| field_struct.new(f["name"], f["type"], f["kind"]) } || []
    )
  end
  File.binwrite t.name, erb.result(binding)
end

task nodes: ["lib/wid/ast/node.rb"]
task tldr: :nodes
task test: :tldr

task default: [:tldr, "standard:fix"]
