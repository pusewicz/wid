# frozen_string_literal: true
# typed: strict

module Wid
  module Nodes
    class AssignmentExpression < Node
      sig { returns(Node) }
      attr_reader :left, :right

      sig { returns(String) }
      attr_reader :operator

      sig { params(operator: String, left: Node, right: Node).void }
      def initialize(operator, left, right)
        @left = T.let(left, Node)
        @operator = T.let(operator, String)
        @right = T.let(right, Node)
      end

      sig { returns(T::Array[Node]) }
      def children = [left, right]

      sig { returns(T::Hash[Symbol, T.untyped]) }
      def to_hash
        {class: self.class, operator: @operator, left: @left.to_hash, right: @right.to_hash}
      end
    end
  end
end
