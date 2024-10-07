module Wid
  module Nodes
    class ExpressionStatement < Node
      attr_reader :expression

      def initialize(expression)
        @expression = expression
        super
      end

      def children
        [@expression]
      end
    end
  end
end
