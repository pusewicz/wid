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
        super("Unrecognized token `#{token.value}'")
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
      parse_assignment_expression
    end

    # AssignmentExpression
    #  : AdditiveExpression
    #  | LeftHandSideExpression AssignmentOperator AssignmentExpression
    #  ;
    def parse_assignment_expression
      left = parse_additive_expression

      return left if !assignment_operator?(peek&.type)

      operator = parse_assignment_operator.value
      left = check_valid_assignment(left)
      right = parse_assignment_expression
      Nodes::AssignmentExpression.new(operator, left, right)
    end

    # LeftHandSideExpression
    #  : Identifier
    #  ;
    def parse_left_hand_side_expression
      parse_identifier
    end

    def check_valid_assignment(node)
      return node if node.is_a?(Nodes::Identifier)

      raise ParserError.new("Invalid left-hand assignment target")
    end

    # Identifier
    #  : IDENTIFIER
    #  ;
    def parse_identifier
      value = consume_type(:IDENTIFIER).value
      Nodes::Identifier.new(value)
    end

    # AssignmentOperator
    #  : SIMPLE_ASSIGN
    #  | COMPLEX_ASSIGN
    #  ;
    def parse_assignment_operator
      if peek.type == :SIMPLE_ASSIGN
        return consume_type(:SIMPLE_ASSIGN)
      end

      consume_type(:COMPLEX_ASSIGN)
    end

    def assignment_operator?(type)
      type == :SIMPLE_ASSIGN || type == :COMPLEX_ASSIGN
    end

    # AdditiveExpression
    #  : MultiplicativeExpression
    #  | AdditiveExpression ADDITIVE_OPERATOR Literal -> Literal ADDITIVE_OPERATOR Literal ADDITIVE_OPERATOR Literal
    #  ;
    def parse_additive_expression
      parse_binary_expression(:multiplicative_expression, :ADDITIVE_OPERATOR)
    end

    # MultiplicativeExpression
    #  : PrimaryExpression
    #  | MultiplicativeExpression MULTIPLICATIVE_OPERATOR PrimaryExpression -> PrimaryExpression MULTIPLICATIVE_OPERATOR PrimaryExpression MULTIPLICATIVE_OPERATOR PrimaryExpression
    #  ;
    def parse_multiplicative_expression
      parse_binary_expression(:primary_expression, :MULTIPLICATIVE_OPERATOR)
    end

    def parse_binary_expression(type, operator_type)
      left = send(:"parse_#{type}")

      while peek&.type == operator_type
        operator = consume_type(operator_type).value
        right = send(:"parse_#{type}")
        left = Nodes::BinaryExpression.new(operator, left, right)
      end

      left
    end

    # PrimaryExpression
    #  : Literal
    #  | ParenthesizedExpression
    #  | LeftHandSideExpression
    #  ;
    def parse_primary_expression
      return parse_literal if literal?(peek.type)

      case peek.type
      when :"(" then parse_parenthesized_expression
      else parse_left_hand_side_expression
      end
    end

    def literal?(type)
      type == :NUMBER || type == :STRING
    end

    # ParenthesizedExpression
    #  : '(' Expression ')'
    #  ;
    def parse_parenthesized_expression
      consume_type(:"(")
      expression = parse_expression
      consume_type(:")")

      expression
    end
  end
end
