module Wid
  module Nodes
    class BooleanLiteral < Node
      attr_reader :value

      def initialize(value)
        @value = value
        super
      end

      def ==(other) = self.class == other.class && value == other.value

      def children = []

      def to_hash = {class: self.class, value: @value}
    end
  end
end
