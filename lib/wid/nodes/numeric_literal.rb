module Wid
  module Nodes
    class NumericLiteral < Node
      attr_reader :value

      def initialize(value)
        @value = value.include?(".") ? Float(value) : Integer(value)
      end

      def to_hash
        {class: self.class, value: @value}
      end
    end
  end
end
