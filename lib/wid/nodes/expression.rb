module Wid
  module Nodes
    class Expression < Node
      attr_reader :value

      def initialize(value = nil)
        @value = value
      end

      def ==(other) = self.class == other.class && value == other.value

      def children = []

      def to_hash = {class: self.class, value: @value}
    end
  end
end
