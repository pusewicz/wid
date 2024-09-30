require "set"

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
      send("visit_#{node.class.name.split('::').last.gsub(/([^\^])([A-Z])/,'\1_\2').downcase}", node)
    end

    def visit_program(node)
      node.children.each { |node| visit(node) }
    end

    def visit_identifier(node)
      node.value
    end

    def visit_string(node)
      node.value
    end

    def visit_binary_operator(node)
      @output << "(#{visit(node.left)} #{node.operator} #{visit(node.right)});"
    end

    def visit_number(node) = node.value.to_s

    def visit_function_call(node)
      case node.function_name
      when 'print', 'puts'
        @headers << 'stdio.h'
      end
      args = node.arguments.map { visit(_1) }.join(', ')
      @output << "#{node.function_name}(#{args});"
    end
    # def visit_expression(node) = node.value

  end
end
