module Wid
  module Nodes
    class Literal < Node
      attr_reader :value

      def initialize(value)
        @value = value
      end

      def ==(other)
        self.class == other.class && value == other.value
      end

      def children
        []
      end

      def to_hash
        {class: self.class, value: @value}
      end
    end
  end
end
