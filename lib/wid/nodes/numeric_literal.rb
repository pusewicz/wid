module Wid
  module Nodes
    class NumericLiteral < Literal
      def initialize(value)
        parsed_value = value.include?(".") ? Float(value) : Integer(value)
        super(parsed_value)
      end

      def to_hash
        {class: self.class, value: value}
      end
    end
  end
end
