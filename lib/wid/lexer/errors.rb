# typed: strict

module Wid
  class Lexer
    LexerError = Class.new(StandardError)

    class SyntaxError < LexerError
      extend T::Sig

      sig { returns(Integer) }
      attr_reader :line, :column

      sig { params(token: T.nilable(String), line: Integer, column: Integer).void }
      def initialize(token, line, column)
        @token = token
        @line = line
        @column = column
        super("Syntax error at line #{line}, column #{column}: unexpected token `#{token.inspect}'")
      end
    end
  end
end
