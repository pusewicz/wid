module Wid
  # Codegen is a class that generates code from a Wid AST using a visitor.
  class Codegen
    MAIN = <<~C
      int main() {
        %s

        return 0;
      }
    C

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

    def generate_headers = @headers.uniq.map { "#include <#{_1}>" }.join("\n") + "\n\n"

    def generate_output = MAIN % @output.join("\n")

    def visit(node)
      unless node.is_a?(Nodes::Node)
        raise "Cannot visit `#{node.inspect}'"
      end
      send("visit_#{node.class.name.split("::").last.gsub(/([^\^])([A-Z])/, '\1_\2').downcase}", node)
    end

    def visit_program(node) = node.children.each { |node| visit(node) }

    def visit_identifier(node) = node.name

    def visit_string(node) = node.value

    def visit_binary_operator(node)
      @output << "(#{visit(node.left)} #{node.operator} #{visit(node.right)});"
    end

    def visit_number(node) = node.value.to_s

    def visit_nil(_node) = @output << "nil;"

    def visit_function_call(node)
      case node.function_name
      when "print", "puts"
        @headers << "stdio.h"
      end
      args = node.arguments.map { visit(_1) }.join(", ")
      @output << "#{node.function_name}(#{args});"
    end

    def visit_var_binding(node)
      type = infer(node.right)
      formatted_type_decl = if type[-1] == "*"
        "#{type.sub("*", "")} *"
      else
        type + " "
      end
      @output << "#{formatted_type_decl}#{node.left.name} = #{visit(node.right)};"
    end

    def infer(node)
      case node
      when Nodes::Number
        node.value.is_a?(Integer) ? "int" : "double"
      when Nodes::String
        "char*"
      when Nodes::BinaryOperator
        if node.children.any? { |child| !child.value.is_a?(Integer) }
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
