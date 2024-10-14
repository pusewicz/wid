module Wid
  module Nodes
    class StringLiteral < Literal
      def initialize(value)
        # Skip the quotes
        parsed_value = value[1...-1]
        super(parsed_value)
      end

      def to_hash
        {class: self.class, value: value}
      end
    end
  end
end
