# frozen_string_literal: true
# typed: strict

require_relative "lexer/errors"
require_relative "lexer/token"

require "strscan"

module Wid
  class Lexer
    extend T::Sig

    SPEC = T.let([
      # New line
      [/^[\n]/, :NEW_LINE],

      # Whitespace
      [/^\s+/, nil],

      # Comments
      [/^#.*[\n]?/, nil],

      # Symbols
      [/^;/, :";"],
      [/^\{/, :"{"],
      [/^\}/, :"}"],
      [/^\(/, :"("],
      [/^\)/, :")"],
      [/^,/, :","],
      [/^\./, :"."],
      [/^\[/, :"["],
      [/^\]/, :"]"],
      [/^\./, :"."],
      [/^@/, :"@"],

      # Keywords
      [/^\btrue\b/, :true],
      [/^\bfalse\b/, :false],
      [/^\bnil\b/, :nil],
      [/^\bdo\b/, :do],
      [/^\bend\b/, :end],
      [/^\bif\b/, :if],
      [/^\bthen\b/, :then],
      [/^\belse\b/, :else],
      [/^\bwhile\b/, :while],
      [/^\bdef\b/, :def],
      [/^\breturn\b/, :return],
      [/^\bclass\b/, :class],
      [/^\bsuper\b/, :super],
      [/^\bself\b/, :self],

      # Numbers
      [/^\d+(\.\d+)?/, :NUMBER],

      # Identifiers
      [/^[_A-Za-z][_0-9A-Za-z]*\b/, :IDENTIFIER],

      # Equality operators: ==, !=
      [/^[=!]=/, :EQUALITY_OPERATOR],

      # Assignment operators: =, *=, /=, +=, -=
      [/^=/, :SIMPLE_ASSIGN],
      [/^[\*\/\+\-]=/, :COMPLEX_ASSIGN],

      # Math operators: +, -, *, /
      [/^[+\-]/, :ADDITIVE_OPERATOR],
      [/^[*\/]/, :MULTIPLICATIVE_OPERATOR],

      # Relational operators: <, <=, >, >=
      [/^[><]=?/, :RELATIONAL_OPERATOR],

      # Logical operators
      [/^&&/, :LOGICAL_AND],
      [/^\|\|/, :LOGICAL_OR],
      [/^!/, :LOGICAL_NOT],

      # Strings: double and single-quoted
      [/^"[^"]*"|'[^']*'/, :STRING]
    ].freeze, T::Array[[Regexp, T.nilable(Symbol)]])

    sig { params(input: String).returns(T::Array[Token]) }
    def self.tokenize(input)
      new(input).tokenize
    end

    sig { params(input: String).void }
    def initialize(input)
      @scanner = T.let(StringScanner.new(input), StringScanner)
      @last_pos = T.let(0, Integer)
      @line_number = T.let(1, Integer)
    end

    sig { returns(T::Array[Token]) }
    def tokenize
      tokens = []

      while (token = next_token)
        tokens << token
      end

      tokens.freeze
    end

    sig { returns(T.nilable(Token)) }
    def next_token
      return if @scanner.eos?

      SPEC.each do |(pattern, type)|
        if (match = @scanner.scan(pattern))
          case type
          when nil then next
          when :NEW_LINE
            @last_pos = @scanner.pos
            @line_number += 1
            next
          else return token(type, match)
          end
        end
      end

      return if @scanner.eos?

      raise SyntaxError.new(@scanner.getch, @line_number, column_number)
    end

    private

    sig { params(type: Symbol, value: T.nilable(String)).returns(Token) }
    def token(type, value = nil)
      Token.new(type: type, value: value, line: @line_number, column: column_number)
    end

    sig { returns(Integer) }
    def column_number
      @scanner.pos - @last_pos - @scanner.matched_size.to_i + 1
    end
  end
end
