module Wid
  # Codegen is a class that generates code from a Wid AST using a visitor.
  class Codegen
    MAIN = %(int main() {\n%s\nreturn 0;\n}\n).freeze

    def initialize
      @headers = %w[Wid.h].to_set
      @output = []
    end

    def self.generate(node) = new.generate(node)

    def generate(node)
      visit(node)

      generate_headers + generate_output
    end

    private

    def generate_headers = @headers.uniq.map { "#include <#{_1}>" }.join("\n") << "\n\n"

    def generate_output = MAIN % (@output.join(";\n") << ";")

    def visit(node)
      raise "Cannot visit `#{node.inspect}'" unless node.is_a?(Nodes::Node)

      method_name = "visit_#{node.class.name.split("::").last.gsub(/([^\^])([A-Z])/, '\1_\2').downcase}"

      send(method_name, node)
    end

    def visit_all(nodes) = nodes.map { visit(_1) }

    def visit_program(node) = @output = visit_all(node.expressions)

    def visit_identifier(node) = node.name

    def visit_string(node) = node.value

    def visit_binary_operator(node)
      "(#{visit(node.left)} #{node.operator} #{visit(node.right)})"
    end

    def visit_number(node) = node.value.to_s

    def visit_nil(_node) = "nil"

    def visit_function_call(node)
      case node.function_name
      when "print", "printf", "puts"
        @headers << "stdio.h"
      end
      args = node.arguments.map { visit(_1) }.join(", ")
      "#{node.function_name}(#{args})"
    end

    def visit_var_binding(node)
      type = infer(node.right)
      formatted_type_decl = if type[-1] == "*"
        "#{type.sub("*", "")} *"
      else
        type + " "
      end
      "#{formatted_type_decl}#{node.var_name_as_str} = #{visit(node.right)}"
    end

    # TODO: Move type inference into a separate AST pipeline pass
    def infer(node)
      case node
      when Nodes::Number
        node.value.is_a?(Integer) ? "int" : "double"
      when Nodes::String
        "char*"
      when Nodes::BinaryOperator
        if node.children.any? { |child| child.value.is_a?(Float) }
          "double"
        else
          "int"
        end
      else
        raise("Cannot infer type of `#{node.inspect}'")
      end
    end
  end
end
