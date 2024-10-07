module Wid
  module Nodes
    class Expression < Node
      attr_reader :value

      def initialize(value = nil)
        @value = value
        super
      end

      def ==(other) = self.class == other.class && value == other.value

      def children = []
    end
  end
end
