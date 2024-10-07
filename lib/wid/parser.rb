# frozen_string_literal: true

require_relative "nodes"

module Wid
  class Parser
    ParserError = Class.new(StandardError)

    UnexpectedTokenError = Class.new(ParserError) do
      attr_reader :token

      def initialize(token, next_token, expected = nil)
        @token = token
        super("Unexpected token #{token.type} `#{token.value}'. Expected `#{expected || next_token.type}'")
      end
    end

    UnrecognizedTokenError = Class.new(ParserError) do
      attr_reader :token

      def initialize(token)
        @token = token
        super("Unrecognized token #{token.type} `#{token.value}'")
      end
    end

    def initialize(tokens)
      @tokens = tokens
      @pos = 0
    end

    def self.parse(tokens) = new(tokens).parse

    def parse
      parse_program
    end

    private

    def consume(offset = 1)
      token = peek(offset)
      @pos += offset
      token
    end

    def consume_type(expected_type)
      if peek&.type == expected_type
        consume
      else
        raise UnexpectedTokenError.new(current_token, peek, expected_type)
      end
    end

    def peek(offset = 1)
      pos = (@pos - 1) + offset
      return if pos < 0 || pos >= @tokens.size

      @tokens[pos]
    end

    def current_token = peek(0)

    # Main entry point
    #
    # Program
    #  : StatementList
    #  ;
    def parse_program
      Nodes::Program.new(parse_statement_list)
    end

    # Literal
    #  : NumericLiteral
    #  | StringLiteral
    #  ;
    def parse_literal
      case peek.type
      when :NUMBER then parse_numeric_literal
      when :STRING then parse_string_literal
      else raise UnrecognizedTokenError.new(peek)
      end
    end

    def parse_numeric_literal
      value = consume_type(:NUMBER).value
      Nodes::NumericLiteral.new(value)
    end

    def parse_string_literal
      value = consume_type(:STRING).value
      Nodes::StringLiteral.new(value)
    end

    # StatementList
    #  : Statement
    #  | StatementList Statement -> Statement Statement Statement Statement
    #  ;
    def parse_statement_list(terminator = nil)
      statements = [parse_statement]

      while peek && peek.type != terminator
        statements << parse_statement
      end

      statements
    end

    # Statement
    #  : ExpressionStatement
    #  : BlockStatement
    #  ;
    def parse_statement
      case peek.type
      when :";" then parse_empty_statement
      when :"{" then parse_block_statement(:"{", :"}")
      when :do then parse_block_statement(:do, :end)
      else parse_expression_statement
      end
    end

    # EmptyStatement
    #  : ';'
    #  ;
    def parse_empty_statement
      consume_type(:";")
      Nodes::EmptyStatement.new
    end

    # BlockStatement
    #  : '{' StatementList? '}'
    #  | do StatementList? end
    #  ;
    def parse_block_statement(opening, closing)
      consume_type(opening)
      statements = (peek.type != closing) ? parse_statement_list(closing) : []
      consume_type(closing)
      Nodes::BlockStatement.new(statements)
    end

    # ExpressionStatement
    #  : Expression
    #  | Expression ';'
    #  ;
    def parse_expression_statement
      expression = parse_expression
      consume_type(:";") if peek&.type == :";"
      Nodes::ExpressionStatement.new(expression)
    end

    # Expression
    #  : Literal
    #  ;
    def parse_expression
      parse_literal
    end
  end
end
