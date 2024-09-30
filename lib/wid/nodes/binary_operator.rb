module Wid
  module Nodes
    class BinaryOperator < Expression
      attr_reader :operator
      attr_accessor :left, :right

      def initialize(operator, left = nil, right = nil)
        @operator = operator
        @left = left
        @right = right
      end

      def ==(other)
        @operator == other&.operator && children == other&.children
      end

      def children
        [@left, @right]
      end
    end
  end
end
