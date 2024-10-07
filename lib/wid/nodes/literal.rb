module Wid
  module Nodes
    class Literal < Node
      attr_reader :value

      def initialize(value)
        @value = value
        super
      end

      def ==(other)
        self.class == other.class && value == other.value
      end

      def children
        []
      end
    end
  end
end
