module Wid
  class Lexer
    LexerError = Class.new(StandardError)

    class SyntaxError < LexerError
      attr_reader :line, :column

      def initialize(token, line, column)
        @token = token
        @line = line
        @column = column
        super("Syntax error at line #{line}, column #{column}: unexpected token `#{token.inspect}'")
      end
    end
  end
end
