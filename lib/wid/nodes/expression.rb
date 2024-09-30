module Wid
  module Nodes
    class Expression
      attr_reader :value

      def initialize(value) = @value = value
      # TODO Both implementations below are temporary. Expression SHOULD NOT have a concrete implementation of these methods.
      def ==(other) = self.class == other.class
      def children = []
      def accept(visitor) = visitor.visit(self)
    end
  end
end
