module Wid
  module Nodes
    class VarBinding < Expression
      attr_accessor :left, :right

      def initialize(left, right)
        @left = left
        @right = right
        super()
      end

      # The instance variable @left is an Nodes::Identifier.
      def var_name_as_str = left.name

      def ==(other) = children == other&.children

      def children = [left, right]
    end
  end
end
