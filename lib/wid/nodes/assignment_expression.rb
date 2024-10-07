module Wid
  module Nodes
    class AssignmentExpression < Node
      attr_reader :left, :right, :operator

      def initialize(operator, left, right)
        @left = left
        @operator = operator
        @right = right
        super
      end

      def children = [left, right]
    end
  end
end
