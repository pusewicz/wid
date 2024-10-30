# frozen_string_literal: true

module Wid
  module Nodes
    class AssignmentExpression < Node
      attr_reader :left, :right

      attr_reader :operator

      def initialize(operator, left, right)
        @left = left
        @operator = operator
        @right = right
      end

      def children = [left, right]

      def to_hash
        {class: self.class, operator: @operator, left: @left.to_hash, right: @right.to_hash}
      end
    end
  end
end
