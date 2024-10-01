module Wid
  module Nodes
    class Expression < Node
      attr_reader :value

      def initialize(value = nil) = @value = value

      # TODO Both implementations below are temporary. Expression SHOULD NOT have a concrete implementation of these methods.
      def ==(other) = self.class == other.class

      def children = []
    end
  end
end
