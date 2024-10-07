module Wid
  class Lexer
    Token = Data.define(:type, :value, :line, :column) do
      def ==(other)
        type == other.type && value == other.value
      end

      def deconstruct
        [type, value]
      end

      def deconstruct_keys(keys)
        {type: type, value: value}
      end
    end
  end
end
