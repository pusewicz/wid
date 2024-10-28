module Wid
  class Lexer
    Token = Data.define(:type, :value, :line, :column) do
      def ==(other)
        type == other.type && value == other.value
      end

      def inspect
        if type.size == 1
          value.inspect
        elsif type.size == value.size
          "`#{type.inspect}'"
        else
          "#{type.inspect}(#{value.inspect})"
        end
      end
    end
  end
end
