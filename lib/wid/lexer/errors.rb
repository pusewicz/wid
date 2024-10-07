module Wid
  class Lexer
    LexerError = Class.new(StandardError)
    class SyntaxError < LexerError
      attr_reader :line, :column

      def initialize(msg, line, column)
        super(msg)
        @line = line
        @column = column
      end
    end
  end
end
