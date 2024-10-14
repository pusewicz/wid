module Wid
  module Nodes
    class BinaryExpression < Node
      attr_reader :left, :right, :operator

      def initialize(operator, left, right)
        @left = left
        @operator = operator
        @right = right
        super
      end

      def children = [left, right]

      def to_hash
        {class: self.class, operator: @operator, left: @left.to_hash, right: @right.to_hash}
      end
    end
  end
end
