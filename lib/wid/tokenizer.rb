# frozen_string_literal: true

require "strscan"

module Wid
  class Tokenizer
    Token = Data.define(:type, :value, :line) do
      def to_a
        [type, value].compact
      end
    end

    IGNORE = /[ \c\r\t]+/
    INT = /(?:[0]|[1-9][0-9]*)/
    FLOAT_DECIMAL = /[.][0-9]+/
    FLOAT_EXP = /[eE][+-]?[0-9]+/
    LITERALS = %w[( )]
    OPERATORS = %w[+ - > < * / ! =]
    TWO_CHAR_OPERATORS = %w[>= <= == !=]
    KEYWORDS = %w[print if else end].freeze
    SINGLE_QUOTED_STRING = /'[^']*'/
    DOUBLE_QUOTED_STRING = /"[^"]*"/
    IDENTIFIER = /[a-zA-Z_]\w*[!?=]?/

    SPEC = {
      NUMBER: /#{INT}(#{FLOAT_DECIMAL}#{FLOAT_EXP}|#{FLOAT_DECIMAL}|#{FLOAT_EXP})?/,
      LITERAL: /[#{LITERALS.sort.join}]/,
      OPERATOR: /#{Regexp.union(TWO_CHAR_OPERATORS)}|[#{OPERATORS.sort.join}]/,
      STRING: /#{SINGLE_QUOTED_STRING}|#{DOUBLE_QUOTED_STRING}/,
      BOOL: /true|false/,
      NIL: /nil/,
      KEYWORD: /#{KEYWORDS.sort.join("|")}/,
      IDENTIFIER: IDENTIFIER,
      NEWLINE: /[\n]/
    }.freeze

    def initialize(string)
      @string = string
      @scan = StringScanner.new string
      @start = @scan.pos
      @line = 1
    end

    def next_token
      @scan.skip(IGNORE)

      return if @scan.eos?

      @start = @scan.pos

      SPEC.each do |type, regexp|
        if (value = @scan.scan(regexp))
          return case type
                 when :OPERATOR, :LITERAL then Token.new(type: value.to_sym, value: nil, line: @line)
                 when :NIL then Token.new(type:, value: nil, line: @line)
                 when :KEYWORD then Token.new(type: value.upcase.to_sym, value: nil, line: @line)
                 when :NEWLINE
                   @line += 1
                   Token.new(type: value.to_sym, value: nil, line: @line)
                 else
                   Token.new(type:, value:, line: @line)
                 end
        end
      end

      raise "Unrecognized token #{@scan.getch.inspect} at line #{@line}"
    end

    def self.tokenize(string)
      tokenizer = new(string)
      tokens = []
      while (token = tokenizer.next_token)
        tokens << token
      end

      tokens
    end

    def line
      @scan.string[0, @scan.pos].count("\n") + 1
    end
  end
end
