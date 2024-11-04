# frozen_string_literal: true

module Wid
  # Codegen is a class that generates code from a Wid AST using a visitor.
  module Codegen
    class C
      MAIN = <<~MAIN
        #include "wid.h"

        int Wid__main() {
          %s;
          return 0;
        }

        int main() {
          return Wid__main();
        }
      MAIN

      def initialize
        @headers = Set.new

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
        klass_name = node.class.name.split("::").last
        method_name = "visit_#{klass_name.gsub(/([^\^])([A-Z])/, '\1_\2').downcase}"

        send(method_name, node)
      end

      def visit_all(nodes) = nodes.map { visit(_1) }

      def visit_program_node(node) = @output = visit_all(node.statements.body)

      def visit_expression_statement(node) = visit(node.expression)

      def visit_identifier(node) = node.name

      def visit_string_node(node) = "\"#{node.unescaped}\""

      def visit_binary_expression(node)
        "(#{visit(node.left)} #{node.operator} #{visit(node.right)})"
      end

      def visit_integer_node(node) = node.value.to_s

      def visit_nil(_node) = "nil"

      def visit_assignment_expression(node)
        type = infer(node.right)
        formatted_type_decl = if type[-1] == "*"
          "#{type.sub("*", "")} *"
        else
          type + " "
        end
        "#{formatted_type_decl}#{visit(node.left)} #{node.operator} #{visit(node.right)}"
      end

      def visit_print_node(node)
        @headers << "stdio.h"
        "printf(#{node.expressions.map { visit(_1) }.join(", ")})"
      end

      # TODO: Move type inference into a separate AST pipeline pass
      def infer(node)
        case node
        when Nodes::NumericLiteral
          node.value.is_a?(Integer) ? "int" : "double"
        when Nodes::StringLiteral
          "char*"
        when Nodes::BinaryExpression
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
end
