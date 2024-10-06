# frozen_string_literal: true

require "strscan"

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

    Token = Data.define(:type, :value, :line, :column) do
      def ==(other) = type == other.type && value == other.value
    end

    TOKEN_TYPES = {
      WHITESPACE: /[ \r\t]+/, # Has to go first
      KEYWORD: /#{Regexp.union(%w[true false nil def end].sort)}\b/,
      NUMBER: /\d+(\.\d+)?/,
      STRING: /"[^"]*"|'[^']*'/,
      IDENTIFIER: /[_A-Za-z][_0-9A-Za-z]*\b/,
      PUNCTUATION: Regexp.union(%w[{ } ( ) [ ] = ! | & + - , .]).freeze,
      NEW_LINE: /[\n]/
    }.freeze

    def self.tokenize(input)
      new(input).tokenize
    end

    def initialize(input)
      @scanner = StringScanner.new(input)
      @last_pos = 0
      @line_number = 0
    end

    def tokenize
      tokens = []

      while (token = next_token)
        tokens << token
      end

      tokens << token(:EOF)
    end

    def next_token
      return if @scanner.eos?

      TOKEN_TYPES.each do |type, pattern|
        if (match = @scanner.scan(pattern))

          case type
          when :WHITESPACE then next
          when :KEYWORD then return token(match.upcase.to_sym, match)
          when :PUNCTUATION, :OPERATOR then return token(match.to_sym, match)
          when :NEW_LINE
            return token(match.to_sym, match).tap do
              @last_pos = @scanner.pos
              @line_number += 1
            end
          else return token(type, match)
          end
        end
      end

      raise SyntaxError.new("Unknown token `#{@scanner.getch}'", @line_number + 1, column_number)
    end

    private

    def token(type, value = nil)
      Token.new(type: type, value: value, line: @line_number, column: column_number)
    end

    def column_number
      @scanner.pos - @last_pos - @scanner.matched_size.to_i
    end
  end
end
