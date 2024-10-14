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

      def to_hash
        {class: self.class, expression: @expression.to_hash}
      end
    end
  end
end
